//! Feature `document`: build-order B6 (SMB-GUI-MENU-1) on a real document. The nineteen titles open popups of the
//! designed tree (harness-view `menu_tree`, presentation only); ONE availability evaluation, `Viewer::studio_menu`,
//! feeds this menu, the egui shell's records (`state.shell.menu`) and the drive surface (MENU-INV-4); every node is
//! drawn with that record as node properties; greyed rows answer their reason and run nothing; a remedy is offered only
//! when running it makes its node available (STU-SHL-020's worked example in a page-layout document); leaves run
//! through the catalog command's own executor (`menu_tree::run_catalog`) or the shell command; aliases show and run
//! their primary (MENU-INV-2); every catalog command has a leaf or a declared palette_only reason and every registry
//! domain a menu home (STU-SHL-013, MENU-INV-1, MENU-INV-3); the components.json keyboard and pointer contract (Left,
//! Right, type-ahead, F10, Alt, Shift+F10, the Menu key, the safe triangle, the scroll lane); tooltips L0 and L1 read
//! the same record, never cover their control, never swallow a click and go on a move, Escape or the next action. The
//! last test draws an open cascade and an L1 tip through the Vulkan UI pass (GPU group, validation clean) and keeps
//! the capture when HSK_NATIVE_HOST_GUI_OUT is set.
#![cfg(feature = "document")]
mod common;

use common::{Scratch, fixture};
use hsk_studio_harness_view::view::Viewer;
use hsk_studio_harness_view::view::menu_tree::descriptors::{AvailabilityState, MenuNodeKind, ReasonCode};
use hsk_studio_harness_view::view::menu_tree::{self, Clause, MenuContext, MenuState, Profile, Unreached};
use hsk_studio_harness_view::view::render::Prefer;
use hsk_studio_harness_view::view::shell::{Panel, ShellCommand};
use hsk_studio_native_host::event::{Button, HostEvent, Key, Mods};
use hsk_studio_native_host::gui::{self, CpuChrome, GuiFrame, Outcome, preset, row_id};
use hsk_studio_native_host::script::{Step, chord, click};
use kumihimo_ui::checks;
use kumihimo_ui::geom::Rect;
use kumihimo_ui::semantic::{SemanticNode, stable_id, valid_author_id};
use kumihimo_ui::tree::Role;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

fn open_path(path: &Path, w: u32, h: u32) -> GuiFrame {
    let mut f = GuiFrame::open(path, w, h, Some(1000)).unwrap_or_else(|e| panic!("gui open: {e}"));
    f.frame().expect("first frame");
    f
}
fn open(dir: &Path, w: u32, h: u32) -> GuiFrame {
    open_path(&fixture(dir), w, h)
}
/// A page-layout document: a one-page PDF opens as a carried, empty document (no layers, no artboard), then `page.add`
/// gives it one US Letter spread, so its profile signature is `[page_spread]`, named shape `layout` (STU-SHL-054);
/// saved as `layout.handshake`. (`document.new` with profile `layout` takes a harness-core type this crate's tests do
/// not link: lane notes.)
fn layout_doc(dir: &Path) -> PathBuf {
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>",
        "<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 612 792] >>",
        "<< /Type /Page /Parent 2 0 R /Contents 4 0 R >>",
        "<< /Length 7 >>\nstream\n0 0 m S\nendstream",
    ];
    let mut pdf = b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n".to_vec();
    let mut offsets = Vec::new();
    for (i, body) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.extend(format!("{} 0 obj\n{body}\nendobj\n", i + 1).bytes());
    }
    let xref_at = pdf.len();
    pdf.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).bytes());
    for at in offsets {
        pdf.extend(format!("{at:010} 00000 n \n").bytes());
    }
    pdf.extend(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_at}\n%%EOF\n", objects.len() + 1).bytes());
    let source = dir.join("one-page.pdf");
    std::fs::write(&source, pdf).unwrap();
    let mut v = Viewer::new(Prefer::Cpu);
    v.open(&source).unwrap_or_else(|e| panic!("open pdf: {e}"));
    v.add_page(None, true, Some((612.0, 792.0)), None).unwrap_or_else(|e| panic!("page.add: {e}"));
    let out = dir.join("layout.handshake");
    v.save(&out, false).unwrap_or_else(|e| panic!("save layout: {e}"));
    out
}
fn invoke(f: &mut GuiFrame, id: &str) -> Outcome {
    let o = f.invoke(id).unwrap_or_else(|e| panic!("{id}: {e}")).unwrap_or_else(|| panic!("{id}: no outcome"));
    f.frame().unwrap();
    o
}
fn ok(command: &'static str) -> Outcome {
    Outcome { command, code: "ok".into() }
}
fn walk<'a>(ns: &'a [SemanticNode], out: &mut Vec<&'a SemanticNode>) {
    for n in ns {
        out.push(n);
        walk(&n.children, out);
    }
}
fn nodes(f: &GuiFrame) -> Vec<SemanticNode> {
    let s = f.semantic();
    let mut v = Vec::new();
    walk(&s.roots, &mut v);
    v.into_iter().cloned().collect()
}
fn sem(f: &GuiFrame, id: &str) -> SemanticNode {
    nodes(f).into_iter().find(|n| n.author_id == id).unwrap_or_else(|| panic!("no semantic node {id}"))
}
fn has(f: &GuiFrame, id: &str) -> bool {
    nodes(f).iter().any(|n| n.author_id == id)
}
fn aid(id: &str) -> String {
    menu_tree::node_author_id(id)
}
fn run(f: &mut GuiFrame, steps: &[Step]) -> Option<Outcome> {
    let mut last = None;
    for s in steps {
        if let Some(o) = f.step(s).unwrap_or_else(|e| panic!("{s:?}: {e}")) {
            last = Some(o);
        }
    }
    f.frame().unwrap();
    last
}
fn key(f: &mut GuiFrame, k: Key) -> Option<Outcome> {
    run(f, &chord(k, Mods::default()))
}
fn key_with(f: &mut GuiFrame, k: Key, mods: Mods) -> Option<Outcome> {
    run(f, &chord(k, mods))
}
fn move_to(f: &mut GuiFrame, x: i32, y: i32) {
    run(f, &[Step::Event(HostEvent::PointerMoved { x, y })]);
}
/// The highlighted menu row of the deepest open popup (hover or keyboard; a row whose cascade is open is drawn selected
/// too, and the deepest popup comes last), if any.
fn hot(f: &GuiFrame) -> Option<SemanticNode> {
    nodes(f).into_iter().rev().find(|n| n.selected && matches!(n.role, Role::MenuItem | Role::MenuItemCheckBox) && n.author_id.starts_with("menu.studio."))
}
fn layer_id(f: &GuiFrame, name: &str) -> String {
    f.rows().iter().find(|r| r.name == name).map(|r| r.layer_id.clone()).unwrap_or_else(|| panic!("no layer {name}"))
}
/// The CPU-painted frame.
fn pixels(f: &mut GuiFrame) -> (u32, Vec<u8>) {
    let mut cpu = CpuChrome::default();
    f.compose(&mut cpu).unwrap();
    let c = cpu.canvas.unwrap();
    (c.width(), c.rgba().to_vec())
}
/// How many pixels of `r` (logical = physical at scale 1) differ between two frames of width `w`.
fn differing(a: &[u8], b: &[u8], w: u32, r: Rect) -> usize {
    let (x0, y0, x1, y1) = (r.x0.max(0.0) as usize, r.y0.max(0.0) as usize, r.x1 as usize, r.y1 as usize);
    let w = w as usize;
    let mut n = 0;
    for y in y0..y1 {
        for x in x0..x1.min(w) {
            let i = (y * w + x) * 4;
            if a.get(i..i + 4) != b.get(i..i + 4) {
                n += 1;
            }
        }
    }
    n
}

/// STU-SHL-011, STU-SHL-015, STU-SHL-017 (MENU-INV-2, -5), STU-SHL-021: the designed tree parses once into nineteen
/// titles in the closed order; the evaluated bar on a real document passes the menu audit; every author id is a valid,
/// collision-free tree id; every catalog leaf runs a command of the one catalog; each catalog command and each shell
/// leaf has ONE primary leaf, every further appearance carries `alias_of` (MENU-INV-2).
#[test]
fn designed_tree_parses_and_the_evaluated_bar_passes_the_audit() {
    let s = Scratch::new("menu-tree");
    let f = open(&s.0, 1920, 1080);
    let t = menu_tree::template();
    let ids: Vec<&str> = t.bar.titles.iter().map(|n| n.id.as_str()).collect();
    assert_eq!(ids, menu_tree::TITLES);
    let labels: Vec<&str> = t.bar.titles.iter().map(|n| n.label.as_str()).collect();
    assert_eq!(labels, preset::MENU);
    // The spec's listing (STU-SHL-023..037 with MOTION > Keyframes' leaves) plus four shell titles; the spec header's
    // counts differ from its own listing (lane notes SD: the listing is the source, lead ruling Q6).
    assert_eq!(t.bar.walk().len(), 1665);
    let bar = f.menu_bar();
    assert_eq!(bar.audit(), Vec::new(), "menu audit");
    let walked = bar.walk();
    assert!(walked.iter().all(|w| w.depth <= menu_tree::descriptors::MAX_MENU_DEPTH));
    let mut stable = BTreeSet::new();
    for w in &walked {
        let a = aid(&w.node.id);
        assert!(valid_author_id(&a), "{a}");
        assert!(stable.insert(stable_id(&a)), "stable id collision {a}");
        if !w.node.availability.is_available() {
            assert!(w.node.availability.reason_code.is_some() && w.node.availability.reason_text.as_deref().is_some_and(|t| !t.is_empty()), "{a}");
        }
    }
    // Every catalog leaf runs a command of the one catalog (no parallel registry, STU-SHL-016).
    let catalog = f.viewer().ui_catalog().expect("ui catalog");
    for l in menu_tree::LEAVES {
        assert!(bar.find(l.id).is_some_and(|n| n.kind == MenuNodeKind::Item && n.alias_of.is_none()), "{} is a primary designed leaf", l.id);
        assert!(catalog.resolve(l.command).is_some(), "{} -> {} in the catalog", l.id, l.command);
        assert!(menu_tree::parse_requires(l.requires).is_ok(), "{}", l.requires);
    }
    for (id, r) in menu_tree::NODE_REQUIRES {
        assert!(bar.find(id).is_some() && menu_tree::parse_requires(r).is_ok(), "{id}");
    }
    // MENU-INV-2: one primary leaf per command (a catalog command with the same parameter rule, or a shell command);
    // the other appearances name their primary.
    let mut primary: BTreeMap<String, String> = BTreeMap::new();
    for w in &walked {
        let n = w.node;
        if n.kind != MenuNodeKind::Item {
            continue;
        }
        let command = match (menu_tree::leaf(&n.id), ShellCommand::from_id(&n.id)) {
            (Some(l), _) => format!("{} {:?}", l.command, l.rule),
            (None, Some(_)) => n.id.clone(),
            _ => continue,
        };
        if n.alias_of.is_none() {
            assert!(primary.insert(command.clone(), n.id.clone()).is_none(), "{command}: a second primary leaf {}", n.id);
        }
    }
    for (alias, of) in menu_tree::ALIASES {
        let n = bar.find(alias).unwrap_or_else(|| panic!("{alias} is a designed node"));
        assert_eq!(n.alias_of.as_deref(), Some(*of), "{alias}");
        assert!(bar.find(of).is_some_and(|p| p.alias_of.is_none()), "{of} is a primary");
    }
    assert_eq!(menu_tree::ALIASES.len(), 4);
    let meta = &t.meta;
    assert!(meta["studio.edit.clear"].derived && !meta["studio.history.undo"].derived);
    assert!(meta["studio.motion.keyframe"].expansion && meta["studio.workspace.panel"].expansion);
    assert!(meta["studio.object.layer.blending_mode"].enumeration);
    for id in ["studio.help.full_command_index", "studio.help.diagnostics", "studio.automate.command_api", "studio.automate.model_lane"] {
        assert!(bar.find(id).is_some_and(|n| n.menu_only), "{id} menu_only");
    }
}

/// STU-SHL-049: twelve closed clause kinds, closed value sets, typed refusals, bounded input; an unmet `tool:` clause
/// is NEEDS_TOOL with a select-the-tool remedy (lead ruling Q7).
#[test]
fn requires_clauses_are_a_closed_set_with_typed_refusals() {
    let all = [
        "container:timeline",
        "layer_kind:raster",
        "doc_feature:pixel_canvas",
        "selection:>=1",
        "selection_kind:group",
        "selection_count:2",
        "tool:paint_brush",
        "capability:fs.write",
        "capability_flag:gpu",
        "mode:read_only",
        "color_mode:cmyk",
        "task_scope:null",
    ];
    let kinds: Vec<&str> = all.iter().map(|t| Clause::parse(t).unwrap().kind()).collect();
    assert_eq!(kinds, menu_tree::CLAUSE_KINDS);
    for t in all {
        assert_eq!(Clause::parse(t).unwrap().text(), t, "round trip");
    }
    let code = |t: &str| Clause::parse(t).unwrap_err().code;
    assert_eq!(code("has_timeline:true"), "requires_kind_unknown", "has_timeline is container:timeline");
    assert_eq!(code("page_count:3"), "requires_kind_unknown", "page count is a query, not a gate");
    assert_eq!(code("container:gallery"), "requires_value_invalid");
    assert_eq!(code("capability_flag:quantum"), "requires_value_invalid");
    assert_eq!(code("color_mode:rgba"), "requires_value_invalid");
    assert_eq!(code("selection:1"), "requires_value_invalid");
    assert_eq!(code("selection_count:-1"), "requires_value_invalid");
    assert_eq!(code("layer_kind:Raster Layer"), "requires_value_invalid");
    assert_eq!(code("container"), "requires_kind_unknown");
    assert_eq!(code(&format!("doc_feature:{}", "x".repeat(10_000))), "requires_too_long");
    let joined = vec!["doc_feature:a"; 13].join(" & ");
    assert_eq!(menu_tree::parse_requires(&joined).unwrap_err().code, "requires_too_long");
    assert_eq!(menu_tree::parse_requires("container:artboard & ").unwrap_err().code, "requires_empty");
    assert_eq!(menu_tree::parse_requires("container:artboard & selection:>=1").unwrap().len(), 2);
    // The predicate: every unmet clause with its code and generated text (articles agree); the remedy table names ONE
    // command.
    let state = MenuState { profile: Profile { containers: [menu_tree::ContainerKind::Artboard].into(), ..Profile::default() }, ..MenuState::default() };
    let unmet = menu_tree::evaluate(&menu_tree::parse_requires("container:artboard & container:timeline").unwrap(), &state);
    assert_eq!(unmet.len(), 1);
    assert_eq!((unmet[0].code, unmet[0].text.as_str()), (ReasonCode::NoSuchContainer, "needs a timeline container"));
    assert_eq!(menu_tree::remedy_of(&unmet[0].clause).as_deref(), Some("studio.motion.timeline_doc.new_timeline"));
    let empty = MenuState::default();
    assert_eq!(menu_tree::evaluate(&menu_tree::parse_requires("container:artboard").unwrap(), &empty)[0].text, "needs an artboard container");
    assert!(menu_tree::evaluate(&[], &state).is_empty());
    // tool: is NEEDS_TOOL (Q7) with the select-the-tool remedy.
    let tool = menu_tree::evaluate(&menu_tree::parse_requires("tool:paint_brush").unwrap(), &state);
    assert_eq!((tool[0].code, tool[0].code.code()), (ReasonCode::NeedsTool, "NEEDS_TOOL"));
    assert_eq!(menu_tree::remedy_of(&tool[0].clause).as_deref(), Some("studio.workspace.tool.paint_brush"));
    let with_tool = MenuState { tool: Some("paint_brush".into()), ..state.clone() };
    assert!(menu_tree::evaluate(&menu_tree::parse_requires("tool:paint_brush").unwrap(), &with_tool).is_empty());
    // The two host codes beyond the twelve (SD notes): NEEDS_TOOL and NOT_IN_THIS_HOST.
    assert_eq!(ReasonCode::ALL.len(), 12);
    let extra: Vec<&str> = ReasonCode::HOST_EXTRA.iter().map(|c| c.code()).collect();
    assert_eq!(extra, ["NEEDS_TOOL", "NOT_IN_THIS_HOST"]);
}

/// B6 done_when, part 1 (AC-REACH-04): all nineteen titles open by pointer and by id; every node of each title's
/// popup is in the tree at every level whatever its state (STU-SHL-018), each with its availability record as node
/// properties (STU-SHL-019); greyed rows are disabled with the reason as value; the four shell titles are greyed and
/// open a line saying why (STU-SHL-052: titles included, lead ruling Q2).
#[test]
fn all_nineteen_titles_open_and_every_node_is_drawn_with_its_record() {
    let s = Scratch::new("menu-titles");
    let mut f = open(&s.0, 1920, 1080);
    let ops = f.viewer().run().ops().len();
    let (x, y) = f.centre("menu.file").unwrap();
    assert_eq!(run(&mut f, &click(x, y)), Some(ok("frame.menu_open")));
    assert_eq!(f.menu_open(), ["file"]);
    assert_eq!(sem(&f, "studio.menu.file").role, Role::Menu);
    assert!(sem(&f, "menu.file").selected, "the open title is drawn selected");
    let (x, y) = f.centre("menu.edit").unwrap();
    move_to(&mut f, x, y);
    assert_eq!(f.menu_open(), ["edit"], "hovering another title switches menus");
    let (x, y) = f.centre(gui::VIEWER_ID).unwrap();
    assert_eq!(run(&mut f, &click(x, y)), Some(ok("frame.menu_close")));
    assert!(f.menu_open().is_empty());
    for (title, label) in menu_tree::TITLES.iter().zip(preset::MENU) {
        assert_eq!(invoke(&mut f, &aid(title)), ok("frame.menu_open"), "{title}");
        let popup = sem(&f, &format!("studio.menu.{title}"));
        assert_eq!((popup.role, popup.label.as_deref()), (Role::Menu, Some(label)));
        let node = f.menu_bar().titles.iter().find(|n| n.id == *title).unwrap().clone();
        if node.children.is_empty() {
            let t = sem(&f, &aid(title));
            assert!(t.disabled, "{title} greyed");
            let a = t.availability.clone().expect("title availability");
            assert_eq!((a.state.as_str(), a.reason_code.as_deref()), ("NOT_IN_THIS_DOCUMENT", Some("NOT_IMPLEMENTED_YET")));
            let empty = sem(&f, &format!("studio.menu.{title}.empty"));
            assert_eq!(empty.value.as_deref(), Some(node.availability.reason_line().as_str()));
            continue;
        }
        assert!(!sem(&f, &aid(title)).disabled, "{title} opens");
        for c in &node.children {
            let n = sem(&f, &aid(&c.id));
            assert!(matches!(n.role, Role::MenuItem | Role::MenuItemCheckBox), "{}", c.id);
            assert_eq!(n.label.as_deref(), Some(c.label.as_str()));
            let a = n.availability.clone().unwrap_or_else(|| panic!("{} availability", c.id));
            assert_eq!(a.state, c.availability.state.code());
            assert_eq!(n.disabled, !c.availability.is_available(), "{}", c.id);
            if !c.availability.is_available() {
                assert_eq!(n.value.as_deref(), Some(c.availability.reason_line().as_str()), "{}", c.id);
                assert_eq!(a.reason_text, c.availability.reason_text);
            }
            if c.availability.state == AvailabilityState::InapplicableHere {
                let r = sem(&f, &menu_tree::remedy_author_id(&c.id));
                assert!(!r.disabled && r.label.as_deref().unwrap().starts_with("-> "), "{} remedy row", c.id);
            }
        }
        let p = f.panel_of(&format!("studio.menu.{title}")).unwrap();
        assert!(checks::overlapping_controls(p.tree()).is_empty(), "{title}");
        let b = p.tree().get(p.tree().find(&format!("studio.menu.{title}")).unwrap()).bounds;
        assert!(b.w() >= 220.0 && b.w() <= 480.0 && b.y1 <= 1080.0, "{title}: {b:?}");
        let clipped = f.clipped();
        assert!(clipped.is_empty(), "{title}: {clipped:?}");
    }
    assert_eq!(invoke(&mut f, &aid("studio.object.layer")), ok("frame.menu_open"));
    assert_eq!(f.menu_open(), ["object", "studio.object.layer"]);
    assert_eq!(sem(&f, "studio.menu.object.layer").role, Role::Menu);
    assert!(has(&f, &aid("studio.object.layer.duplicate_layer")));
    assert_eq!(f.viewer().run().ops().len(), ops, "opening and browsing ran nothing");
    let st = f.menu_stats();
    assert!(st.opens >= 21 && st.runs == 0, "{st:?}");
}

/// MENU-INV-4 (review F1, lead ruling 1): ONE evaluation. This frame's menu IS `Viewer::studio_menu` with the frame's
/// context; the drive surface's answer is the same function with every shell command shown; for the shell's own leaves
/// both carry `state.shell.menu`'s record, the frame's adjusted only where this window cannot show the command
/// (NOT_IN_THIS_HOST) or cannot run the record's remedy (then NOT_IN_THIS_DOCUMENT with the same reason).
#[test]
fn one_evaluation_feeds_the_native_menu_the_shell_menu_and_the_drive_surface() {
    let s = Scratch::new("menu-one");
    let path = fixture(&s.0);
    let mut f = open_path(&path, 1920, 1080);
    for select in [None, Some("Red")] {
        if let Some(name) = select {
            let id = layer_id(&f, name);
            f.edit(|v| v.select(Some(id)));
        }
        let ctx = f.menu_context();
        let (mine, _) = f.viewer().studio_menu(&ctx);
        let (drive, _) = f.viewer().studio_menu(&MenuContext::everything());
        let shell = f.viewer().state_json()["shell"]["menu"]["nodes"].as_array().unwrap().clone();
        let mut shell_nodes = 0;
        let mut not_here = 0;
        for w in f.menu_bar().walk() {
            let n = w.node;
            let a = &n.availability;
            assert_eq!(a, &mine.find(&n.id).unwrap().availability, "{}: the frame shows its own evaluation", n.id);
            let d = &drive.find(&n.id).unwrap_or_else(|| panic!("{} in the drive evaluation", n.id)).availability;
            match ShellCommand::from_id(&n.id) {
                None => assert_eq!(a, d, "{}: catalog and designed nodes are identical", n.id),
                Some(cmd) => {
                    shell_nodes += 1;
                    let rec = &shell.iter().find(|x| x["id"] == n.id.as_str()).unwrap_or_else(|| panic!("{} in state.shell.menu", n.id))["availability"];
                    assert_eq!(&d.json(), rec, "{}: the drive answer is the shell's record", n.id);
                    if !gui::frame_hosts(cmd) && d.is_available() {
                        not_here += 1;
                        assert_eq!((a.state, a.reason_code), (AvailabilityState::NotInThisDocument, Some(ReasonCode::NotInThisHost)), "{}", n.id);
                        assert_eq!(a.reason_text.as_deref(), Some(menu_tree::NOT_IN_THIS_HOST_TEXT));
                    } else if a.state != d.state {
                        // The record's remedy is a command this window does not show: the same reason, no remedy.
                        assert_eq!((d.state, a.state), (AvailabilityState::InapplicableHere, AvailabilityState::NotInThisDocument), "{}", n.id);
                        assert_eq!((a.reason_code, &a.reason_text, &a.remedy_command_id), (d.reason_code, &d.reason_text, &None), "{}", n.id);
                        let r = d.remedy_command_id.as_deref().unwrap();
                        assert!(ShellCommand::from_id(r).is_some_and(|c| !gui::frame_hosts(c)), "{}: remedy {r}", n.id);
                    } else {
                        assert_eq!(a, d, "{}", n.id);
                    }
                }
            }
        }
        assert!(shell_nodes >= 30 && not_here >= 5, "{shell_nodes} shell nodes, {not_here} not in this host");
    }
    // The run path refuses a greyed leaf with the record the frame shows (MENU-INV-4).
    let (bar, state) = (f.menu_bar().clone(), f.menu_state().clone());
    let mut refused = None;
    f.edit(|v| refused = Some(v.run_menu_leaf_in(&bar, &state, "studio.object.ungroup").unwrap_err()));
    let refused = refused.unwrap();
    assert_eq!(refused.availability.as_ref(), Some(&bar.find("studio.object.ungroup").unwrap().availability));
    assert_eq!(refused.code, "wrong_selection_kind");
}

/// B6 done_when, part 2: a greyed item shows its reason and runs nothing (STU-SHL-018, STU-SHL-019): the status bar
/// says it, the outcome is the reason code, the menu stays open. Save As is a shell command this window does not show
/// (NOT_IN_THIS_HOST, lead ruling 2); Undo with nothing to undo is the shell's own record.
#[test]
fn a_greyed_item_answers_its_reason_and_runs_nothing() {
    let s = Scratch::new("menu-greyed");
    let mut f = open(&s.0, 1920, 1080);
    let ops = f.viewer().run().ops().len();
    invoke(&mut f, "menu.file");
    let save_as = aid("studio.document.save_as");
    assert!(sem(&f, &save_as).disabled);
    let (x, y) = f.centre(&save_as).unwrap();
    assert_eq!(run(&mut f, &click(x, y)), Some(Outcome { command: "frame.menu_unavailable", code: "not_in_this_host".into() }));
    assert_eq!(f.menu_open(), ["file"], "the menu stays open on a greyed row");
    let status = f.node("studio.status.command").unwrap();
    assert_eq!(status.label.as_deref(), Some("Save As…: not shown in this window"));
    assert_eq!(status.value.as_deref(), Some("frame.menu_unavailable not_in_this_host"));
    let undo = f.menu_bar().find("studio.history.undo").unwrap().availability.clone();
    assert_eq!((undo.state, undo.reason_code, undo.reason_text.as_deref()), (AvailabilityState::NotInThisDocument, Some(ReasonCode::DocFeatureAbsent), Some("nothing to undo")));
    let redo = f.menu_bar().find("studio.history.redo").unwrap().availability.clone();
    assert_eq!((redo.state, redo.remedy_command_id), (AvailabilityState::NotInThisDocument, None), "Redo names no remedy (lead ruling Q3)");
    let mut bar = f.menu_bar().clone();
    let state = f.menu_state().clone();
    let mut out = Vec::new();
    f.edit(|v| {
        out.push(v.run_menu_leaf_in(&bar, &state, "studio.history.undo").unwrap_err());
        out.push(v.run_menu_leaf_in(&bar, &state, "studio.object.layer").unwrap_err());
        out.push(v.run_menu_leaf_in(&bar, &state, "studio.document.save_as").unwrap_err());
    });
    assert_eq!((out[0].code.as_str(), out[0].availability.as_ref().map(|a| a.state)), ("doc_feature_absent", Some(AvailabilityState::NotInThisDocument)));
    assert_eq!(out[1].code, "menu_not_a_leaf");
    assert_eq!(out[2].code, "not_in_this_host");
    bar.titles.clear();
    let mut gone = None;
    f.edit(|v| gone = Some(v.run_menu_leaf_in(&bar, &state, "studio.history.undo").unwrap_err().code));
    assert_eq!(gone.as_deref(), Some("menu_node_unknown"));
    assert_eq!(f.viewer().run().ops().len(), ops, "greyed rows reach no command");
    assert_eq!(f.menu_stats().unavailable, 1);
}

/// B6 done_when, part 3, STU-SHL-020's worked example as the spec states it, in a page-layout document (review F2):
/// MOTION > Keyframes is INAPPLICABLE_HERE NO_SUCH_CONTAINER "needs a timeline container" with DOCUMENT > Timelines >
/// New Timeline as its remedy row; the remedy runs `video.project_new_sequence` with the built-in preset that best
/// matches the document at 24 fps (lead ruling 4, parameters in the receipt), Keyframes is then available, the profile
/// adds `timeline` (STU-SHL-048, signature STU-SHL-054); save and reopen keep it; an undo of that step takes the
/// container away explicitly (the profile is the document's own content: lane notes reading).
#[test]
fn the_worked_example_holds_in_a_page_layout_document() {
    let s = Scratch::new("menu-worked");
    let path = layout_doc(&s.0);
    let mut f = open_path(&path, 1920, 1080);
    assert_eq!(f.menu_state().profile.signature(), ["page_spread"]);
    assert_eq!(f.menu_state().profile.named_shape(), Some("layout"));
    let kf = f.menu_bar().find("studio.motion.keyframe").unwrap().availability.clone();
    assert_eq!(
        (kf.state, kf.reason_code, kf.reason_text.as_deref(), kf.remedy_command_id.as_deref()),
        (AvailabilityState::InapplicableHere, Some(ReasonCode::NoSuchContainer), Some("needs a timeline container"), Some("studio.motion.timeline_doc.new_timeline"))
    );
    assert!(f.menu_bar().find("studio.motion.timeline_doc.new_timeline").unwrap().availability.is_available(), "New Timeline needs no artboard");
    let track = f.menu_bar().find("studio.motion.tracks.add_track").unwrap().availability.clone();
    assert_eq!(track.reason_code, Some(ReasonCode::NotImplementedYet), "Add Track… is unbound (dialog pending, lead ruling 4)");
    invoke(&mut f, "menu.motion");
    let remedy = menu_tree::remedy_author_id("studio.motion.keyframe");
    assert_eq!(sem(&f, &remedy).label.as_deref(), Some("-> New Timeline"));
    let want_preset = menu_tree::new_timeline_preset(f.viewer()).expect("a 24 fps built-in preset");
    assert_eq!(want_preset, "legacy-arri-1080p-arri-1080p-24", "no preset at the page size: the spec default 1920 x 1080 at 24 fps");
    let undo0 = f.viewer().run().history()["undo"].as_array().unwrap().len();
    let changes = f.menu_stats().profile_changes;
    let (x, y) = f.centre(&remedy).unwrap();
    assert_eq!(run(&mut f, &click(x, y)), Some(ok("video.project_new_sequence")));
    assert!(f.menu_open().is_empty(), "a leaf that ran closes the menus");
    assert_eq!(f.menu_state().profile.signature(), ["page_spread", "timeline"]);
    assert_eq!(f.menu_state().profile.named_shape(), Some("layout"));
    assert_eq!(f.menu_stats().remedies, 1);
    assert_eq!(f.menu_stats().profile_changes, changes + 1);
    assert!(f.menu_bar().find("studio.motion.keyframe").unwrap().availability.is_available(), "the remedy made Keyframes available");
    assert_eq!(f.viewer().run().history()["undo"].as_array().unwrap().len(), undo0 + 1, "one undo step");
    let receipt = f.receipt().render();
    let want = format!(r#""last_run":{{"command":"video.project_new_sequence","params":{{"name":"Timeline","preset_id":"{want_preset}"}}}}"#);
    assert!(receipt.contains(&want), "{want} in {receipt}");
    let actor = f.viewer().run().actor();
    let step = f.viewer().run().history()["undo_steps"].as_array().unwrap().last().unwrap().clone();
    assert_eq!(step["actor"]["account_id"].as_str(), Some(actor.account_id()), "{step}");
    // Save and reopen: the timeline is the document's, so the reopened profile declares it at once.
    let out = s.0.join("worked.handshake");
    f.edit(|v| v.save(&out, false).unwrap());
    let g = open_path(&out, 1280, 720);
    assert_eq!(g.menu_state().profile.signature(), ["page_spread", "timeline"]);
    assert!(g.menu_bar().find("studio.motion.keyframe").unwrap().availability.is_available());
    // Undo removes the sequence; the profile follows the document (an explicit removal, never a silent one).
    f.edit(|v| {
        v.undo().unwrap();
    });
    assert!(f.viewer().run().snapshot().unwrap().document().sequences.is_empty());
    assert_eq!(f.menu_state().profile.signature(), ["page_spread"]);
    assert_eq!(f.menu_stats().profile_changes, changes + 2);
    assert_eq!(f.menu_bar().find("studio.motion.keyframe").unwrap().availability.state, AvailabilityState::InapplicableHere);
}

/// Review F6 (lead ruling 1): a remedy is offered only when running it makes its node available. For every
/// INAPPLICABLE_HERE catalog or designed node on the fixture (nothing selected, then a raster layer selected), a fresh
/// frame on the same document runs the remedy through the menu and the node is then AVAILABLE. A remedy that could not
/// do that is not offered (Ungroup with nothing selected: Group itself needs a selection; Redo names none).
#[test]
fn every_remedy_makes_its_node_available() {
    let s = Scratch::new("menu-remedies");
    let path = fixture(&s.0);
    let mut checked = 0;
    for select in [None, Some("Red")] {
        let prepare = |f: &mut GuiFrame| {
            if let Some(name) = select {
                let id = layer_id(f, name);
                f.edit(|v| v.select(Some(id)));
            }
        };
        let mut f = open_path(&path, 1280, 720);
        prepare(&mut f);
        let targets: Vec<(String, String)> = f
            .menu_bar()
            .walk()
            .iter()
            .filter(|w| w.node.availability.state == AvailabilityState::InapplicableHere && ShellCommand::from_id(&w.node.id).is_none())
            .map(|w| (w.node.id.clone(), w.node.availability.remedy_command_id.clone().unwrap()))
            .collect();
        assert!(targets.len() >= 2, "{select:?}: {targets:?}");
        for (id, remedy) in targets {
            let mut g = open_path(&path, 1280, 720);
            prepare(&mut g);
            assert_eq!(g.menu_bar().find(&id).unwrap().availability.remedy_command_id.as_deref(), Some(remedy.as_str()));
            let o = invoke(&mut g, &menu_tree::remedy_author_id(&id));
            assert_eq!(o.code, "ok", "{id}: remedy {remedy}: {o:?}");
            assert!(g.menu_bar().find(&id).unwrap().availability.is_available(), "{select:?}: {id} after its remedy {remedy}: {:?}", g.menu_bar().find(&id).unwrap().availability);
            checked += 1;
        }
    }
    assert!(checked >= 8, "{checked}");
    // Not offered: a remedy that does not make the predicate true.
    let f = open_path(&path, 1280, 720);
    let ungroup = f.menu_bar().find("studio.object.ungroup").unwrap().availability.clone();
    assert_eq!((ungroup.state, ungroup.reason_code, ungroup.remedy_command_id), (AvailabilityState::NotInThisDocument, Some(ReasonCode::WrongSelectionKind), None));
    assert_eq!(f.menu_bar().find("studio.history.redo").unwrap().availability.remedy_command_id, None);
}

/// Items dispatch typed commands: SELECT > Navigate Selection > First Object (view state), then OBJECT > Layer >
/// Duplicate Layer, EDIT > Duplicate (an alias), OBJECT > Group and Ungroup, Hide Selection, Send Backward: each reports
/// its catalog command id, each mutation is one named undo step, EDIT > Undo (its chord resolved at render) reverts,
/// and the result reaches save and export.
#[test]
fn leaves_run_catalog_commands_as_undo_steps_that_reach_save_and_export() {
    let s = Scratch::new("menu-run");
    let mut f = open(&s.0, 1920, 1080);
    let rows0 = f.rows().len();
    let dup = f.menu_bar().find("studio.object.layer.duplicate_layer").unwrap().availability.clone();
    assert_eq!((dup.reason_code, dup.remedy_command_id.as_deref()), (Some(ReasonCode::NeedsSelection), Some("studio.select.navigate_selection.first_object")));
    assert_eq!(invoke(&mut f, &aid("studio.select.navigate_selection.first_object")), ok("layer.select"));
    let top = f.rows()[0].layer_id.clone();
    assert_eq!(f.selected(), Some(top.as_str()));
    assert_eq!(invoke(&mut f, &aid("studio.select.navigate_selection.next_object_below")), ok("layer.select"));
    assert_eq!(f.selected(), Some(f.rows()[1].layer_id.as_str()));
    assert_eq!(invoke(&mut f, &aid("studio.select.navigate_selection.next_object_above")), ok("layer.select"));
    assert_eq!(invoke(&mut f, &aid("studio.select.navigate_selection.next_object_above")), Outcome { command: "layer.select", code: "no_next_object".into() });
    let undo0 = f.viewer().run().history()["undo"].as_array().unwrap().len();
    assert_eq!(invoke(&mut f, &aid("studio.object.layer.duplicate_layer")), ok("layer.duplicate_layer"));
    assert_eq!(f.rows().len(), rows0 + 1);
    assert_eq!(invoke(&mut f, &aid("studio.edit.duplicate")), ok("layer.duplicate_layer"));
    assert_eq!(invoke(&mut f, &aid("studio.object.group")), ok("layer.group_layers"));
    let group = f.selected().unwrap().to_owned();
    assert_eq!(f.rows().iter().find(|r| r.layer_id == group).unwrap().kind, "group");
    assert!(f.menu_bar().find("studio.object.ungroup").unwrap().availability.is_available(), "a group is selected");
    assert_eq!(invoke(&mut f, &aid("studio.object.ungroup")), ok("layer.ungroup"));
    assert_eq!(invoke(&mut f, &aid("studio.select.navigate_selection.first_object")), ok("layer.select"));
    let mut runs = 8u64;
    while !f.rows().iter().find(|r| Some(r.layer_id.as_str()) == f.selected()).unwrap().visible {
        assert_eq!(invoke(&mut f, &aid("studio.select.navigate_selection.next_object_below")), ok("layer.select"));
        runs += 1;
    }
    let hidden = f.selected().unwrap().to_owned();
    assert_eq!(invoke(&mut f, &aid("studio.object.hide.hide_selection")), ok("layer.set_visible"));
    assert!(!f.rows().iter().find(|r| r.layer_id == hidden).unwrap().visible);
    assert_eq!(invoke(&mut f, &aid("studio.object.arrange.send_backward")), ok("layer.shift_layer"));
    let history = f.viewer().run().history();
    assert_eq!(history["undo"].as_array().unwrap().len(), undo0 + 6, "six mutations, six steps: {}", history["undo"]);
    invoke(&mut f, "menu.edit");
    let undo = sem(&f, &aid("studio.history.undo"));
    assert!(!undo.disabled);
    let p = f.panel_of(&aid("studio.history.undo")).unwrap();
    let words: Vec<String> = p
        .tree()
        .nodes
        .iter()
        .filter_map(|n| match &n.paint {
            kumihimo_ui::tree::Paint::Label { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert!(words.iter().any(|w| w == "Ctrl+Z"), "chord drawn: {words:?}");
    assert_eq!(invoke(&mut f, &aid("studio.history.undo")), ok("studio.history.undo"));
    assert!(f.menu_bar().find("studio.history.redo").unwrap().availability.is_available());
    let rows = f.rows().len();
    let out = s.0.join("menu-run.handshake");
    f.edit(|v| v.save(&out, false).unwrap());
    let png = s.0.join("menu-run.png");
    f.edit(|v| {
        v.export(&png, hsk_studio_harness_view::view::export::ExportFormat::Png, false).unwrap();
    });
    assert!(std::fs::metadata(&png).unwrap().len() > 0);
    let g = GuiFrame::open(&out, 1280, 720, Some(1000)).unwrap();
    assert_eq!(g.rows().len(), rows);
    assert_eq!(f.menu_stats().runs, runs + 3);
    assert_eq!(f.menu_stats().refused, 1);
}

/// Review F3 (lead ruling 5): a menu leaf runs the catalog command's own executor. Two frames on one document: one
/// runs each leaf through its menu, the other's Viewer runs `menu_tree::run_catalog` with the leaf's parameters; after
/// every step the undo names, the selection and the layers are identical.
#[test]
fn menu_leaves_and_the_shared_executor_leave_the_same_document() {
    let s = Scratch::new("menu-executor");
    let path = fixture(&s.0);
    let mut a = open_path(&path, 1280, 720);
    let mut b = open_path(&path, 1280, 720);
    let leaves = [
        "studio.select.navigate_selection.first_object",
        "studio.select.navigate_selection.next_object_below",
        "studio.object.layer.duplicate_layer",
        "studio.object.group",
        "studio.object.ungroup",
        "studio.object.hide.hide_selection",
        "studio.object.arrange.bring_forward",
        "studio.object.arrange.send_backward",
        "studio.object.layer.new",
        "studio.object.layer.delete_layer",
        "studio.select.navigate_selection.last_object",
        "studio.select.deselect",
        "studio.motion.timeline_doc.new_timeline",
    ];
    for id in leaves {
        let o = invoke(&mut a, &aid(id));
        let l = menu_tree::leaf(id).unwrap();
        assert_eq!(o.command, l.command, "{id}");
        let state = b.menu_state().clone();
        let mut res = None;
        b.edit(|v| {
            let params = menu_tree::leaf_params(v, l, &state).unwrap_or_else(|e| panic!("{id}: {e:?}"));
            res = Some(menu_tree::run_catalog(v, l.command, &params).expect("a catalog command"));
        });
        let res = res.unwrap().unwrap_or_else(|e| panic!("{id}: {e:?}"));
        assert_eq!(o.code == "ok", res.is_ok(), "{id}: {o:?} vs {res:?}");
        let (ha, hb) = (a.viewer().run().history(), b.viewer().run().history());
        assert_eq!(ha["undo"], hb["undo"], "{id}: undo names");
        assert_eq!(a.selected(), b.selected(), "{id}: selection");
        let names = |f: &GuiFrame| f.rows().iter().map(|r| (r.name.clone(), r.kind.clone(), r.visible)).collect::<Vec<_>>();
        assert_eq!(names(&a), names(&b), "{id}: layers");
    }
    assert!(a.viewer().run().history()["undo"].as_array().unwrap().len() >= 8);
}

/// MENU-INV-2: an alias leaf shows its primary's record and runs its primary's command.
#[test]
fn aliases_show_and_run_their_primary() {
    let s = Scratch::new("menu-alias");
    let mut f = open(&s.0, 1280, 720);
    for (alias, of) in menu_tree::ALIASES {
        assert_eq!(f.menu_bar().find(alias).unwrap().availability, f.menu_bar().find(of).unwrap().availability, "{alias}");
    }
    assert_eq!(invoke(&mut f, &aid("studio.insert.layer.pixel_layer")), ok("layer.new_raster_layer"));
    assert!(f.menu_bar().find("studio.history.step_backward").unwrap().availability.is_available());
    assert_eq!(invoke(&mut f, &aid("studio.history.step_backward")), ok("studio.history.undo"));
    assert_eq!(invoke(&mut f, &aid("studio.history.step_forward")), ok("studio.history.redo"));
    assert_eq!(f.viewer().run().history()["undo"].as_array().unwrap().len(), 1);
}

/// STU-SHL-013 / MENU-INV-1 (review F7): every catalog command of the `ui_seed` catalog is run by a leaf or declared
/// palette_only with a typed reason, never both, never neither. MENU-INV-3: every registry domain has a menu home in
/// the designed tree. A command or domain added later fails here until it is placed.
#[test]
fn every_catalog_command_has_a_leaf_or_a_palette_reason_and_every_domain_a_home() {
    let v = Viewer::new(Prefer::Cpu);
    let catalog = v.ui_catalog().expect("ui catalog");
    let commands: BTreeMap<String, String> =
        catalog.rows().iter().filter(|d| d.author_id.starts_with("studio.command.")).filter_map(|d| d.command_id.clone().map(|c| (c, d.domain.clone()))).collect();
    assert!(commands.len() > 400, "{}", commands.len());
    let reached = menu_tree::reached_commands();
    let declared: BTreeMap<&str, Unreached> = menu_tree::PALETTE_ONLY.iter().copied().collect();
    assert_eq!(declared.len(), menu_tree::PALETTE_ONLY.len(), "a command declared twice");
    let missing: Vec<&String> = commands.keys().filter(|c| !reached.contains(c.as_str()) && !declared.contains_key(c.as_str())).collect();
    if !missing.is_empty() {
        let reads = ["summary", "verify", "list", "get", "query", "state", "status", "report", "resolve", "inspect", "read", "checkpoint", "stage", "record", "submit", "describe", "probe"];
        for c in &missing {
            let last = c.rsplit('.').next().unwrap_or(c);
            let plumbing = ["catalog.", "command.", "action."].iter().any(|p| c.starts_with(p)) || reads.iter().any(|r| last.starts_with(r)) || last.ends_with("_entry");
            println!("PALETTE    (\"{c}\", Unreached::{}),", if plumbing { "NoMenuHome" } else { "MenuPathPending" });
        }
    }
    let domains: BTreeSet<&str> = commands.values().map(String::as_str).collect();
    let homes: BTreeMap<&str, &str> = menu_tree::DOMAIN_HOMES.iter().copied().collect();
    let homeless: Vec<&&str> = domains.iter().filter(|d| !homes.contains_key(**d)).collect();
    for d in &homeless {
        println!("DOMAIN    (\"{d}\", \"\"),");
    }
    assert!(missing.is_empty(), "{} catalog commands have neither a leaf nor a palette_only reason (STU-SHL-013)", missing.len());
    for (c, _) in menu_tree::PALETTE_ONLY {
        assert!(!reached.contains(c), "{c}: palette_only and reached (no third state)");
        assert!(commands.contains_key(*c), "{c}: declared but not in the catalog");
    }
    for c in &reached {
        assert!(commands.contains_key(*c), "{c}: a leaf runs a command the catalog lacks");
    }
    // MENU-INV-3.
    assert!(homeless.is_empty(), "{} registry domains have no menu home (MENU-INV-3): {homeless:?}", homeless.len());
    let t = menu_tree::template();
    for (d, home) in menu_tree::DOMAIN_HOMES {
        assert!(domains.contains(d), "{d}: a home for a domain the catalog lacks");
        assert!(t.bar.find(home).is_some_and(|n| n.kind != MenuNodeKind::Item), "{d}: home {home} is a designed title or submenu");
    }
    let by_reason = |r: Unreached| menu_tree::PALETTE_ONLY.iter().filter(|(_, x)| *x == r).count();
    println!(
        "MENU-INV-1: {} catalog commands, {} reached by a leaf, {} {}, {} {}; {} domains",
        commands.len(),
        reached.len(),
        by_reason(Unreached::MenuPathPending),
        Unreached::MenuPathPending.code(),
        by_reason(Unreached::NoMenuHome),
        Unreached::NoMenuHome.code(),
        domains.len()
    );
}

/// STU-SHL-014: the Layers context menu (right click on a row, Shift+F10 or the Menu key on the focused or selected
/// row, or by id) lists references to menu-bar leaves only, with the same author ids and availability, and runs the
/// identical command.
#[test]
fn the_layers_context_menu_references_menu_bar_leaves() {
    let s = Scratch::new("menu-ctx");
    let mut f = open(&s.0, 1920, 1080);
    let red = layer_id(&f, "Red");
    let (x, y) = f.centre(&row_id(&red)).unwrap();
    let steps = [
        Step::Event(HostEvent::PointerMoved { x, y }),
        Step::Event(HostEvent::Button { button: Button::Right, pressed: true }),
        Step::Event(HostEvent::Button { button: Button::Right, pressed: false }),
    ];
    assert_eq!(run(&mut f, &steps), Some(ok("frame.menu_open")));
    assert_eq!(f.menu_open(), [gui::LAYERS_CONTEXT]);
    assert_eq!(f.selected(), Some(red.as_str()), "the row under the pointer is selected");
    let popup = sem(&f, "studio.ctx.layers");
    assert_eq!(popup.role, Role::Menu);
    let rows: Vec<String> = popup.children.iter().map(|c| c.author_id.clone()).filter(|id| !id.ends_with(".remedy")).collect();
    let want: Vec<String> = preset::LAYER_CONTEXT.iter().map(|id| aid(id)).collect();
    assert_eq!(rows, want);
    for id in preset::LAYER_CONTEXT {
        let n = f.menu_bar().find(id).unwrap_or_else(|| panic!("{id} is a menu-bar node"));
        assert_eq!(n.kind, MenuNodeKind::Item, "{id} is a leaf");
        assert_eq!(sem(&f, &aid(id)).availability.unwrap().state, n.availability.state.code());
    }
    assert_eq!(invoke(&mut f, &aid("studio.object.hide.hide_selection")), ok("layer.set_visible"));
    assert!(!f.rows().iter().find(|r| r.layer_id == red).unwrap().visible);
    // Shift+F10 and the Menu key open it on the selected row, under that row.
    let shift = Mods { shift: true, ..Mods::default() };
    assert_eq!(key_with(&mut f, Key::F10, shift), Some(ok("frame.menu_open")));
    assert_eq!(f.menu_open(), [gui::LAYERS_CONTEXT]);
    let below = sem(&f, "studio.ctx.layers").bounds.y0 >= f.node(&row_id(&red)).unwrap().bounds.y1 - 0.5;
    assert!(below, "the context menu opens under its row");
    key(&mut f, Key::Escape);
    assert!(f.menu_open().is_empty());
    assert_eq!(key(&mut f, Key::ContextMenu), Some(ok("frame.menu_open")));
    assert_eq!(f.menu_open(), [gui::LAYERS_CONTEXT]);
    // Keyboard inside it: Down highlights the first row, Enter runs it (Duplicate Layer on Red).
    key(&mut f, Key::Down);
    assert_eq!(hot(&f).map(|n| n.author_id), Some(aid(preset::LAYER_CONTEXT[0])));
    let rows0 = f.rows().len();
    assert_eq!(key(&mut f, Key::Enter), Some(ok("layer.duplicate_layer")));
    assert_eq!(f.rows().len(), rows0 + 1);
    // Nothing selected: the keys answer that there is no row to open it on.
    invoke(&mut f, &aid("studio.select.deselect"));
    assert_eq!(key(&mut f, Key::ContextMenu), Some(Outcome { command: "frame.menu", code: "menu_context_none".into() }));
    assert_eq!(f.open_context(&format!("{}.visibility", row_id(&red))), Ok(ok("frame.menu_open")));
    assert_eq!(f.open_context("studio.tab.layers"), Err("menu_context_none"));
    assert_eq!(f.open_context("studio.panel.layers.row.nope"), Err("unknown_layer"));
    let (x, y) = f.centre(gui::VIEWER_ID).unwrap();
    assert_eq!(run(&mut f, &click(x, y)), Some(ok("frame.menu_close")));
}

/// WORKSPACE > Panels (lead ruling 2): the shell's 25 panels with the shell's ids; the ones this frame hosts run its tab
/// pick and the egui shell's `shell_command` for the same id shows the same panel; the others are greyed
/// NOT_IN_THIS_HOST (or keep the shell's own reason); toggle rows are MenuItemCheckBox with their state.
#[test]
fn workspace_panels_list_the_shell_panels_and_pick_the_hosted_tabs() {
    let s = Scratch::new("menu-panels");
    let path = fixture(&s.0);
    let mut f = open_path(&path, 1920, 1080);
    let panels = f.menu_bar().find(gui::WORKSPACE_PANELS).unwrap().clone();
    let ids: Vec<String> = panels.children.iter().map(|c| c.id.clone()).collect();
    let want: Vec<String> = Panel::ALL.iter().map(|p| ShellCommand::Panel(*p).id()).collect();
    assert_eq!(ids, want);
    let hosted: Vec<&str> = Panel::ALL.iter().filter(|p| gui::hosted_panel(p.key()).is_some()).map(|p| p.key()).collect();
    assert_eq!(hosted, ["layers", "properties", "info", "swatches", "masks", "brushes"]);
    let (everything, _) = f.viewer().studio_menu(&MenuContext::everything());
    for c in &panels.children {
        let Some(ShellCommand::Panel(p)) = ShellCommand::from_id(&c.id) else { panic!("{}", c.id) };
        let shell = &everything.find(&c.id).unwrap().availability;
        match (gui::hosted_panel(p.key()), shell.is_available()) {
            (None, true) => assert_eq!(c.availability.reason_code, Some(ReasonCode::NotInThisHost), "{}", c.id),
            _ => assert_eq!((c.availability.reason_code, &c.availability.reason_text), (shell.reason_code, &shell.reason_text), "{}: the shell's own record", c.id),
        }
    }
    invoke(&mut f, "menu.workspace");
    invoke(&mut f, &aid(gui::WORKSPACE_PANELS));
    let info = aid("studio.workspace.panel.info");
    let row = sem(&f, &info);
    assert_eq!((row.role, row.value.as_deref(), row.disabled), (Role::MenuItemCheckBox, Some("off"), false));
    assert_eq!(f.active_tab("studio.group.colour"), Some("colour"));
    assert_eq!(invoke(&mut f, &info), ok("frame.tab_select"));
    assert_eq!(f.active_tab("studio.group.colour"), Some("info"), "the frame shows the Info panel");
    assert!(f.menu_open().is_empty());
    assert!(f.menu_bar().find("studio.workspace.panel.info").unwrap().selected, "its check follows the frame");
    let mut v = Viewer::new(Prefer::Cpu);
    v.open(&path).unwrap();
    assert!(!v.panel_visible(Panel::Info));
    v.shell_command(ShellCommand::from_id("studio.workspace.panel.info").unwrap()).unwrap();
    assert!(v.panel_visible(Panel::Info), "the egui shell shows the same panel for the same id");
    // A panel this frame has no place for is greyed with the typed reason and runs nothing.
    invoke(&mut f, "menu.workspace");
    invoke(&mut f, &aid(gui::WORKSPACE_PANELS));
    let editor = aid("studio.workspace.panel.layout_editor");
    let n = sem(&f, &editor);
    assert!(n.disabled);
    assert_eq!(n.availability.unwrap().reason_code.as_deref(), Some("NOT_IN_THIS_HOST"));
    assert_eq!(invoke(&mut f, &editor), Outcome { command: "frame.menu_unavailable", code: "not_in_this_host".into() });
}

/// FIT-M1 with B6: at 640 the chevron lists the titles it hides (each opens its menu), and a hidden title opens by id.
#[test]
fn the_chevron_lists_hidden_titles_and_each_opens() {
    let s = Scratch::new("menu-more");
    let mut f = open(&s.0, 640, 480);
    let more = f.node(gui::chrome::MORE_ID).expect("chevron at 640").clone();
    let hidden: Vec<String> = more.value.as_deref().unwrap().strip_prefix("hidden=").unwrap().split(',').map(str::to_owned).collect();
    assert!(!hidden.is_empty());
    assert_eq!(invoke(&mut f, gui::chrome::MORE_ID), ok("frame.menu_open"));
    let list: Vec<String> = sem(&f, "studio.menu.more").children.iter().map(|c| c.author_id.clone()).filter(|id| !id.contains(".scroll-")).collect();
    let want: Vec<String> = hidden.iter().map(|h| gui::more_row_id(h.strip_prefix("menu.").unwrap())).collect();
    assert_eq!(list, want);
    let last = hidden.last().unwrap().strip_prefix("menu.").unwrap().to_owned();
    assert_eq!(invoke(&mut f, &gui::more_row_id(&last)), ok("frame.menu_open"));
    assert_eq!(f.menu_open(), [gui::MORE_MENU.to_owned(), last.clone()]);
    assert!(has(&f, &format!("studio.menu.{last}")));
    assert_eq!(invoke(&mut f, gui::chrome::MORE_ID), ok("frame.menu_close"));
    assert_eq!(invoke(&mut f, &aid(&last)), ok("frame.menu_open"));
    let b = f.panel_of(&format!("studio.menu.{last}")).unwrap();
    let r = b.tree().get(b.tree().find(&format!("studio.menu.{last}")).unwrap()).bounds;
    assert!(r.x0 >= 0.0 && r.x1 <= 640.0 && r.y1 <= 480.0, "inside the window: {r:?}");
}

/// components.json menu_popup keyboard and pointer (review F5): Down and Up move through every row (greyed rows too),
/// Right opens the highlighted submenu with its first row highlighted and at the first level steps to the next title,
/// Left closes a cascade and at the first level steps to the previous title, a letter jumps to the next row starting
/// with it, Enter opens or runs, Escape closes the topmost popup; a menu taller than the window scrolls with a scroll
/// lane (arrow buttons) and keeps the rows outside it in the tree with no box.
#[test]
fn keyboard_moves_opens_and_escapes() {
    let s = Scratch::new("menu-keys");
    let mut f = open(&s.0, 1024, 480);
    invoke(&mut f, "menu.file");
    key(&mut f, Key::Down);
    assert!(sem(&f, &aid("studio.document.new")).selected, "the first row is highlighted");
    assert_eq!(key(&mut f, Key::Enter), Some(ok("frame.menu_open")));
    assert_eq!(f.menu_open(), ["file", "studio.document.new"]);
    key(&mut f, Key::Escape);
    assert_eq!(f.menu_open(), ["file"]);
    assert!(sem(&f, &aid("studio.document.new")).selected, "the closed submenu's row keeps the highlight");
    // Right opens the cascade and highlights its first row; Left closes it.
    key(&mut f, Key::Right);
    assert_eq!(f.menu_open(), ["file", "studio.document.new"]);
    assert_eq!(hot(&f).map(|n| n.author_id), Some(aid("studio.document.new.blank_document")));
    key(&mut f, Key::Left);
    assert_eq!(f.menu_open(), ["file"]);
    // At the first level Left and Right step titles (FILE wraps to OPERATOR).
    key(&mut f, Key::Left);
    assert_eq!(f.menu_open(), ["operator"]);
    key(&mut f, Key::Right);
    assert_eq!(f.menu_open(), ["file"]);
    key(&mut f, Key::Right);
    assert_eq!(f.menu_open(), ["edit"]);
    key(&mut f, Key::Left);
    assert_eq!(f.menu_open(), ["file"]);
    // Type-ahead: `s` goes to the next row starting with S, again to the one after.
    key(&mut f, Key::Char('s'));
    let first_s = hot(&f).expect("a row starting with s");
    assert!(first_s.label.as_deref().unwrap().starts_with('S'), "{first_s:?}");
    key(&mut f, Key::Char('s'));
    let second_s = hot(&f).unwrap();
    assert!(second_s.label.as_deref().unwrap().starts_with('S') && second_s.author_id != first_s.author_id);
    // FILE has more rows than fit at 480: the scroll lane, and the highlight scrolls into view.
    let (up, down) = ("studio.menu.file.scroll-up", "studio.menu.file.scroll-down");
    assert_eq!(sem(&f, down).role, Role::Button);
    for _ in 0..3 {
        key(&mut f, Key::Up);
    }
    let h = hot(&f).unwrap();
    assert!(h.bounds.h() > 0.0 && h.bounds.y1 <= 480.0, "{h:?}");
    key(&mut f, Key::Escape);
    invoke(&mut f, "menu.file");
    assert!(sem(&f, up).disabled && !sem(&f, down).disabled, "at the top only down scrolls");
    assert!(sem(&f, &aid("studio.document.new")).bounds.h() > 0.0);
    assert_eq!(invoke(&mut f, down), ok("frame.menu_scroll"));
    assert_eq!(sem(&f, &aid("studio.document.new")).bounds.h(), 0.0, "one row up and out, still in the tree");
    assert!(!sem(&f, up).disabled);
    let (x, y) = f.centre(up).unwrap();
    assert_eq!(run(&mut f, &click(x, y)), Some(ok("frame.menu_scroll")));
    assert!(sem(&f, &aid("studio.document.new")).bounds.h() > 0.0, "back");
    assert_eq!(f.menu_open(), ["file"], "a scroll button keeps the menu open");
    assert_eq!(key(&mut f, Key::Escape), Some(ok("frame.menu_close")));
    assert!(f.menu_open().is_empty());
}

/// components.json menu_bar keyboard (review F5): F10 puts the keyboard on the menu bar (FILE), Left and Right step
/// titles there, Down opens the title with its first row highlighted, Escape leaves; Alt with a letter opens the title
/// starting with it; in bar mode a letter alone does too.
#[test]
fn f10_and_alt_reach_the_menu_bar() {
    let s = Scratch::new("menu-bar-keys");
    let mut f = open(&s.0, 1920, 1080);
    assert_eq!(key(&mut f, Key::F10), None);
    assert_eq!(f.menu_bar_hot(), Some("file"));
    assert!(sem(&f, "menu.file").selected, "the keyboard title is drawn selected");
    key(&mut f, Key::Right);
    assert_eq!(f.menu_bar_hot(), Some("edit"));
    key(&mut f, Key::Left);
    key(&mut f, Key::Left);
    assert_eq!(f.menu_bar_hot(), Some("operator"));
    key(&mut f, Key::Right);
    assert_eq!(key(&mut f, Key::Down), Some(ok("frame.menu_open")));
    assert_eq!((f.menu_open(), f.menu_bar_hot()), (&["file".to_owned()][..], None));
    assert_eq!(hot(&f).map(|n| n.author_id), Some(aid("studio.document.new")));
    key(&mut f, Key::Escape);
    assert!(f.menu_open().is_empty());
    key(&mut f, Key::F10);
    key(&mut f, Key::Escape);
    assert_eq!(f.menu_bar_hot(), None);
    let alt = Mods { alt: true, ..Mods::default() };
    assert_eq!(key_with(&mut f, Key::Char('v'), alt), Some(ok("frame.menu_open")));
    assert_eq!(f.menu_open(), ["view"]);
    assert_eq!(key_with(&mut f, Key::Char('O'), alt), Some(ok("frame.menu_open")));
    assert_eq!(f.menu_open(), ["object"]);
    key(&mut f, Key::Escape);
    key(&mut f, Key::F10);
    assert_eq!(key(&mut f, Key::Char('o')), Some(ok("frame.menu_open")));
    assert_eq!(f.menu_open(), ["object"]);
    // The undo chord still works with the bar idle (no stolen Ctrl+letter).
    key(&mut f, Key::Escape);
    let ctrl = Mods { ctrl: true, ..Mods::default() };
    assert_eq!(key_with(&mut f, Key::Char('z'), ctrl).map(|o| o.command), Some("history.undo"));
}

/// components.json menu_popup pointer (review F5): a submenu row opens its cascade on hover; while the pointer moves
/// from that row toward the cascade (inside the triangle to its near edge) the cascade stays open even across other
/// rows; a straight move onto another row switches.
#[test]
fn the_safe_triangle_keeps_a_cascade_open_while_the_pointer_heads_for_it() {
    let s = Scratch::new("menu-triangle");
    let mut f = open(&s.0, 1920, 1080);
    invoke(&mut f, "menu.object");
    let layer = aid("studio.object.layer");
    let (ax, ay) = f.centre(&layer).unwrap();
    move_to(&mut f, ax, ay);
    assert_eq!(f.menu_open(), ["object", "studio.object.layer"]);
    let parent = sem(&f, "studio.menu.object").bounds;
    let mask = sem(&f, &aid("studio.object.mask")).bounds;
    let my = ((mask.y0 + mask.y1) / 2.0) as i32;
    move_to(&mut f, parent.x1 as i32 - 3, my);
    assert_eq!(f.menu_open(), ["object", "studio.object.layer"], "heading for the cascade across Mask keeps it");
    move_to(&mut f, ax, ay);
    move_to(&mut f, ax, my);
    assert_eq!(f.menu_open(), ["object", "studio.object.mask"], "a straight move down switches");
}

/// Review F4: a greyed row that has the keyboard or hover highlight shows it (surface.row_selected and the accent edge),
/// while its item stays disabled.
#[test]
fn a_highlighted_greyed_row_is_visibly_highlighted() {
    let s = Scratch::new("menu-grey-hot");
    let mut f = open(&s.0, 1920, 1080);
    invoke(&mut f, "menu.edit");
    let undo = aid("studio.history.undo");
    assert!(sem(&f, &undo).disabled);
    let (w, plain) = pixels(&mut f);
    key(&mut f, Key::Down);
    let n = sem(&f, &undo);
    assert!(n.selected && n.disabled, "highlighted and still greyed");
    let (_, lit) = pixels(&mut f);
    let r = Rect::new(n.bounds.x0 - 2.0, n.bounds.y0, n.bounds.x1, n.bounds.y1);
    let changed = differing(&plain, &lit, w, r);
    assert!(changed as f32 > r.w() * r.h() * 0.5, "{changed} of {} px", r.w() * r.h());
    key(&mut f, Key::Down);
    let (_, moved) = pixels(&mut f);
    assert_eq!(differing(&plain, &moved, w, r), 0, "the highlight left the row");
}

/// STU-SHL-230 to 235 (AC-REACH-05): L0 after 400 ms on the hovered control (name and chord only), L1 after 900 ms;
/// L1 reads the summary from the UiDescriptor set (STU-SHL-240: no prose in UI source), the reason the node carries and
/// the one-click remedy, which runs from the pointer; a menu row's tip sits beside its popup; suppressed while a
/// button is held, 500 ms after, on the control just operated, with tips off; no tip from script steps alone.
#[test]
fn tooltips_l0_and_l1_read_the_same_record() {
    let s = Scratch::new("menu-tips");
    let mut f = open(&s.0, 1920, 1080);
    let (x, y) = f.centre("menu.edit").unwrap();
    move_to(&mut f, x, y);
    assert!(f.tip().is_none());
    f.advance(399);
    assert!(f.tip().is_none(), "no tip before the dwell");
    f.advance(1);
    f.frame().unwrap();
    let t = f.tip().expect("L0 at 400 ms");
    assert_eq!((t.level, t.name.as_str(), t.reason.as_deref()), (0, "EDIT", None));
    let tip = sem(&f, gui::chrome::TIP_ID);
    assert_eq!((tip.role, tip.label.as_deref()), (Role::Tooltip, Some("EDIT")));
    let b = sem(&f, "menu.edit").bounds;
    assert!(tip.bounds.y0 >= b.y0 + b.h(), "never over its own control: {:?} {b:?}", tip.bounds);
    f.advance(500);
    assert_eq!(f.tip().unwrap().level, 1, "L1 at 900 ms");
    run(&mut f, &click(x, y));
    assert!(f.tip().is_none());
    f.advance(1000);
    assert!(f.tip().is_none(), "the control just operated");
    // The open EDIT menu: L1 of Redo (dimmed) through the accessibility request, beside the popup.
    let redo = aid("studio.history.redo");
    let r = f.show_tip(&redo, 1).unwrap();
    f.frame().unwrap();
    let node = sem(&f, &redo);
    let a = node.availability.clone().unwrap();
    assert_eq!(r.reason, a.reason_text, "the tip shows the reason the node carries");
    assert_eq!(r.value, node.value, "the tip and the node carry the same reason line");
    assert_eq!(sem(&f, gui::chrome::TIP_ID).value, node.value);
    let popup = sem(&f, "studio.menu.edit").bounds;
    assert!(sem(&f, gui::chrome::TIP_ID).bounds.intersect(&popup).is_empty(), "a menu row's tip covers no row of its popup");
    // The summary is the UiDescriptor's (the shell leaf's help) or absent (catalog commands have none yet).
    let catalog = f.viewer().ui_catalog().unwrap();
    assert_eq!(r.summary, catalog.resolve("studio.history.redo").and_then(|d| d.summary.clone()));
    assert!(r.summary.is_some());
    let l0 = f.show_tip(&aid("studio.history.undo"), 0).unwrap();
    assert_eq!((l0.name.as_str(), l0.chord.as_deref(), l0.reason.as_deref(), l0.summary.as_deref()), ("Undo", Some("Ctrl+Z"), None, None));
    // L1 of Keyframes: reason and remedy; the pointer reaches the remedy button over the tip and clicks it.
    invoke(&mut f, "menu.motion");
    let kf = aid("studio.motion.keyframe");
    let t = f.show_tip(&kf, 1).unwrap();
    assert_eq!((t.reason.as_deref(), t.remedy.as_deref(), t.remedy_label.as_deref()), (Some("needs a timeline container"), Some("studio.motion.timeline_doc.new_timeline"), Some("New Timeline")));
    assert_eq!(t.summary, None, "a designed submenu has no descriptor sentence");
    f.frame().unwrap();
    assert_eq!(sem(&f, gui::chrome::TIP_REMEDY_ID).role, Role::Button);
    let (x, y) = f.centre(gui::chrome::TIP_REMEDY_ID).unwrap();
    move_to(&mut f, x, y);
    assert!(f.tip().is_some(), "moving onto the tip keeps it");
    assert_eq!(run(&mut f, &click(x, y)), Some(ok("video.project_new_sequence")));
    assert!(f.tip().is_none() && f.menu_state().profile.signature().contains(&"timeline"));
    // Tips off (STU-SHL-235 rule 4).
    f.set_tips(true, None);
    let (x, y) = f.centre("menu.view").unwrap();
    move_to(&mut f, x, y);
    f.advance(2000);
    assert!(f.tip().is_none());
    assert_eq!(f.show_tip("studio.bogus.control", 1), Err("unknown_author_id"));
    assert!(f.menu_stats().tips >= 5);
}

/// Review F10 (STU-SHL-235): a tip never swallows a click (a press on it reaches what lies under it, except its remedy
/// button) and goes on a pointer move off its control, on Escape and on the next action (a key, a press, an invoke).
#[test]
fn a_tip_never_swallows_a_click_and_goes_on_move_escape_and_the_next_action() {
    let s = Scratch::new("menu-tip-dismiss");
    let mut f = open(&s.0, 1920, 1080);
    let red = row_id(&layer_id(&f, "Red"));
    f.show_tip(&red, 1).unwrap();
    f.frame().unwrap();
    let tip = sem(&f, gui::chrome::TIP_ID).bounds;
    let under = sem(&f, &red).bounds;
    assert!(tip.intersect(&under).is_empty(), "never over its own control");
    let (x, y) = (((tip.x0 + tip.x1) / 2.0) as i32, ((tip.y0 + tip.y1) / 2.0) as i32);
    let o = run(&mut f, &click(x, y)).expect("the press reached the control under the tip");
    assert!(matches!(o.command, "layer.select" | "layer.set_visible"), "{o:?}");
    assert!(f.tip().is_none(), "the press dismissed the tip");
    // A move off its control.
    f.show_tip("menu.edit", 0).unwrap();
    let (x, y) = f.centre("menu.view").unwrap();
    move_to(&mut f, x, y);
    assert!(f.tip().is_none(), "a move off its control dismisses it");
    // Escape dismisses the tip and nothing else.
    invoke(&mut f, "menu.edit");
    f.show_tip(&aid("studio.history.undo"), 1).unwrap();
    key(&mut f, Key::Escape);
    assert!(f.tip().is_none());
    assert_eq!(f.menu_open(), ["edit"], "Escape took the tip, not the menu");
    // The next key is the next action.
    f.show_tip(&aid("studio.history.undo"), 1).unwrap();
    key(&mut f, Key::Down);
    assert!(f.tip().is_none());
    // An invoke is too.
    f.show_tip("menu.view", 0).unwrap();
    invoke(&mut f, "menu.view");
    assert!(f.tip().is_none());
}

/// STU-SHL-055 (no Layout Preset or fit state gates): the availability of every node is identical across window sizes
/// (FIT ladder states) and active tabs; STU-SHL-046: menus are evaluated per context change, never per frame.
#[test]
fn availability_does_not_depend_on_layout_and_is_not_evaluated_per_frame() {
    let s = Scratch::new("menu-gate");
    let states = |f: &GuiFrame| -> Vec<(String, &'static str)> { f.menu_bar().walk().iter().map(|w| (w.node.id.clone(), w.node.availability.state.code())).collect() };
    let mut a = open(&s.0, 1920, 1080);
    let b = open(&s.0, 640, 480);
    assert_eq!(states(&a), states(&b));
    let before = states(&a);
    a.invoke("studio.tab.channels").unwrap();
    a.frame().unwrap();
    assert_eq!(states(&a), before, "a tab switch changes no availability");
    let evals = a.menu_stats().evaluations;
    invoke(&mut a, "menu.file");
    for _ in 0..10 {
        assert!(!a.frame().unwrap(), "idle frames rebuild nothing with a menu open");
    }
    assert_eq!(a.menu_stats().evaluations, evals, "opening a menu evaluates nothing");
    let receipt = a.receipt().render();
    for want in [r#""menu":{"open":["file"]"#, r#""profile_signature":["artboard"]"#, r#""named_shape":"raster""#, r#""nodes":"#, r#""evaluations":"#, r#""profile_changes":"#, r#""last_run":null"#] {
        assert!(receipt.contains(want), "{want} in {receipt}");
    }
    let json = a.menu_json();
    assert!(json.contains(r#""schema":"hsk.studio.controls.menu@1""#) && json.contains(r#""named_shape":"raster""#), "{}", &json[..200.min(json.len())]);
    let tree = a.tree_json();
    assert!(tree.contains(r#""availability":{"availability_state":"NOT_IN_THIS_DOCUMENT","reason_code":"NOT_IMPLEMENTED_YET""#), "tree");
}

/// Hostile ids and storms: unknown ids are typed refusals; a thousand open and close cycles stay bounded; closed
/// menus leave the paint order of SMB-GUI-FRAME-1 unchanged.
#[test]
fn hostile_ids_and_menu_storms() {
    let s = Scratch::new("menu-hostile");
    let mut f = open(&s.0, 1280, 720);
    assert_eq!(f.invoke("menu.studio.bogus"), Err("unknown_author_id"));
    assert_eq!(f.invoke(&format!("menu.{}", "x".repeat(4096))), Err("unknown_author_id"));
    assert_eq!(f.invoke("studio.menu.more.nope"), Err("unknown_author_id"));
    assert_eq!(f.invoke("menu.studio.history.undo.remedy"), Ok(Some(Outcome { command: "frame.menu", code: "menu_no_remedy".into() })));
    for i in 0..1000 {
        let t = menu_tree::TITLES[i % 19];
        f.invoke(&aid(t)).unwrap();
        if i % 7 == 0 {
            f.frame().unwrap();
        }
    }
    f.frame().unwrap();
    assert!(f.menu_open().len() <= 1);
    key(&mut f, Key::Escape);
    assert!(f.menu_open().is_empty());
    struct Count(usize);
    impl gui::ChromeTarget for Count {
        fn draw(&mut self, c: gui::Chrome<'_>) -> Result<(), hsk_studio_native_host::HostError> {
            self.0 = c.lists.len();
            Ok(())
        }
    }
    let mut c = Count(0);
    f.compose(&mut c).unwrap();
    assert_eq!(c.0, 11, "closed menus add no list");
    invoke(&mut f, "menu.edit");
    f.show_tip("menu.edit", 0).unwrap();
    f.frame().unwrap();
    f.compose(&mut c).unwrap();
    assert_eq!(c.0, 13, "the popup and the tip draw last");
    let mut cpu = CpuChrome::default();
    f.compose(&mut cpu).unwrap();
    assert_eq!((cpu.stats.missing_resources, cpu.stats.glyph_refusals), (0, 0));
}

/// The Vulkan window path (GPU group, review F8): an open cascade with a greyed row and an L1 tip drawn by the UI pass
/// match the CPU painter outside the antialiased rects; the compared frame holds the popups and the tip (their pixels
/// differ from the closed frame's); validation clean (run without VK_INSTANCE_LAYERS). With HSK_NATIVE_HOST_GUI_OUT
/// set, both frames, the closed frame and the tree file are kept there.
#[test]
fn an_open_menu_and_tip_on_the_ui_pass_match_the_cpu_painter() {
    use hsk_studio_native_host::scene::ChromeMode;
    use hsk_studio_vulkan::{GpuClientPort, Owner, OwnerConfig, UiPass};
    let s = Scratch::new("menu-vk");
    let doc = fixture(&s.0);
    let mut owner = Owner::open(OwnerConfig::default()).expect("Vulkan 1.3 device");
    let mut pass = UiPass::new(&owner).expect("ui pass");
    pass.set_trace(true);
    let (w, h) = (1280u32, 720u32);
    let mut f = GuiFrame::open(&doc, w, h, Some(1000)).unwrap();
    f.set_chrome(ChromeMode::Vulkan);
    f.frame().unwrap();
    let (_, closed) = pixels(&mut f);
    f.invoke("menu.object").unwrap();
    f.invoke(&aid("studio.object.layer")).unwrap();
    f.frame().unwrap();
    f.show_tip(&aid("studio.object.layer.duplicate_layer"), 1).unwrap();
    assert!(f.frame().unwrap());
    let pane = owner.create_pane(w, h).unwrap();
    f.mark_upload();
    assert!(f.draw(&mut gui::VulkanChrome { pass: &mut pass, owner: &mut owner, pane }).unwrap());
    let got = owner.read_pane(pane).unwrap();
    owner.release_pane(pane).unwrap();
    let (_, want) = pixels(&mut f);
    // The compared frame holds what this test is about.
    for id in ["studio.menu.object", "studio.menu.object.layer", gui::chrome::TIP_ID] {
        let r = sem(&f, id).bounds;
        let n = differing(&closed, &want, w, r);
        assert!(n as f32 > r.w() * r.h() * 0.3, "{id}: {n} px of {r:?} differ from the closed frame");
    }
    assert!(sem(&f, &aid("studio.object.layer.duplicate_layer")).disabled, "a greyed row is in the frame");
    let (max, n, outside) = gui::parity(&want, &got, w, pass.trace());
    println!("menu parity: max {max} differing {n} outside_aa {outside}");
    if let Some(out) = std::env::var_os("HSK_NATIVE_HOST_GUI_OUT") {
        let out = PathBuf::from(out);
        std::fs::create_dir_all(&out).unwrap();
        hsk_studio_native_host::capture::write_png(&out.join("gui-menu-open-cascade-tip-vulkan.png"), w, h, &got).unwrap();
        hsk_studio_native_host::capture::write_png(&out.join("gui-menu-open-cascade-tip-cpu.png"), w, h, &want).unwrap();
        hsk_studio_native_host::capture::write_png(&out.join("gui-menu-closed-cpu.png"), w, h, &closed).unwrap();
        std::fs::write(out.join("gui-menu-open-cascade-tip.tree.json"), f.tree_file()).unwrap();
        println!("captures in {}", out.display());
    }
    assert_eq!(outside, 0, "pixels outside the AA rects differ");
    // Measured 2026-11-13 (8702ac57, debug, validation on): max 0, 0 differing pixels; the bound is the measurement.
    assert_eq!(max, 0, "{n} px differ");
    drop(pass);
    drop(owner);
    let v = hsk_studio_vulkan::process_validation();
    println!("validation {} errors {} warnings {}", v.state, v.errors, v.warnings);
    if v.state == "on" {
        assert_eq!((v.errors, v.warnings), (0, 0), "{v:?}");
        assert!(v.loader.iter().all(|n| hsk_studio_vulkan::is_expected_loader_note(&n.name, &n.text)), "{v:?}");
    } else {
        println!("NOT_PROVEN: validation layer state {}", v.state);
    }
}
