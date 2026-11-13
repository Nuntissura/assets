//! Feature `document`: build-order B6 (SMB-GUI-MENU-1) on a real document. The nineteen titles open popups of the
//! designed tree from one descriptor source (harness-view `menu_tree`); every node is drawn with ONE availability
//! record as node properties; greyed rows answer their reason and run nothing; the STU-SHL-020 worked example's remedy
//! runs and grows the profile; leaves run catalog commands as undo steps that survive save and reopen; the Layers
//! context menu references menu-bar leaves; tooltips L0 and L1 read the same record; the menu tables and hostile ids
//! are refused with typed codes. The last test draws an open menu and an L1 tip through the Vulkan UI pass (GPU group,
//! validation clean).
#![cfg(feature = "document")]
mod common;

use common::{Scratch, fixture};
use hsk_studio_harness_view::view::menu_tree::descriptors::{AvailabilityState, MenuNodeKind, ReasonCode};
use hsk_studio_harness_view::view::menu_tree::{self, Clause, MenuState, Profile};
use hsk_studio_native_host::event::{Button, HostEvent, Key};
use hsk_studio_native_host::gui::{self, CpuChrome, GuiFrame, Outcome, preset, row_id};
use hsk_studio_native_host::script::{Step, chord, click};
use kumihimo_ui::checks;
use kumihimo_ui::semantic::{SemanticNode, stable_id, valid_author_id};
use kumihimo_ui::tree::Role;

fn open(dir: &std::path::Path, w: u32, h: u32) -> GuiFrame {
    let mut f = GuiFrame::open(&fixture(dir), w, h, Some(1000)).unwrap_or_else(|e| panic!("gui open: {e}"));
    f.frame().expect("first frame");
    f
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
fn aid(id: &str) -> String {
    menu_tree::node_author_id(id)
}
/// Runs leaf `id` of `bar` on the frame's viewer through the menu tree's run path.
fn run_leaf(f: &mut GuiFrame, bar: &menu_tree::descriptors::MenuBar, id: &str) -> Result<menu_tree::MenuRun, menu_tree::MenuRefusal> {
    let mut out = None;
    f.edit(|v| out = Some(menu_tree::run(v, bar, id)));
    out.unwrap()
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

/// STU-SHL-011, STU-SHL-015, STU-SHL-017 (MENU-INV-2, -5), STU-SHL-021: the designed tree parses once into nineteen
/// titles in the closed order; the evaluated bar on a real document passes the menu audit (unique ids, reasons on
/// every greyed node, every remedy an available leaf, depth, no vendor names); every author id is a valid,
/// collision-free tree id; every leaf this host runs reports a catalog command id (no parallel registry, STU-SHL-016).
#[test]
fn designed_tree_parses_and_the_evaluated_bar_passes_the_audit() {
    let s = Scratch::new("menu-tree");
    let f = open(&s.0, 1920, 1080);
    let t = menu_tree::template();
    let ids: Vec<&str> = t.bar.titles.iter().map(|n| n.id.as_str()).collect();
    assert_eq!(ids, menu_tree::TITLES);
    let labels: Vec<&str> = t.bar.titles.iter().map(|n| n.label.as_str()).collect();
    assert_eq!(labels, preset::MENU);
    assert_eq!(t.bar.walk().len(), 1647, "the designed tree of STU-SHL-023..037 plus four shell titles");
    let bar = f.menu_bar();
    assert_eq!(bar.audit(), Vec::new(), "menu audit");
    let walked = bar.walk();
    assert!(walked.iter().all(|w| w.depth <= menu_tree::descriptors::MAX_MENU_DEPTH));
    let mut stable = std::collections::BTreeSet::new();
    for w in &walked {
        let a = aid(&w.node.id);
        assert!(valid_author_id(&a), "{a}");
        assert!(stable.insert(stable_id(&a)), "stable id collision {a}");
        if !w.node.availability.is_available() {
            assert!(w.node.availability.reason_code.is_some() && w.node.availability.reason_text.as_deref().is_some_and(|t| !t.is_empty()), "{a}");
        }
    }
    // Every leaf this host runs reports a command of the one catalog.
    let catalog = f.viewer().ui_catalog().expect("ui catalog");
    for b in menu_tree::BINDINGS {
        assert!(bar.find(b.id).is_some_and(|n| n.kind == MenuNodeKind::Item), "{} is a designed leaf", b.id);
        assert!(catalog.resolve(b.op.command_id()).is_some(), "{} -> {} in the catalog", b.id, b.op.command_id());
        assert!(menu_tree::parse_requires(b.requires).is_ok(), "{}", b.requires);
    }
    for (id, r) in menu_tree::NODE_REQUIRES {
        assert!(bar.find(id).is_some() && menu_tree::parse_requires(r).is_ok(), "{id}");
    }
    let meta = &t.meta;
    assert!(meta["studio.edit.clear"].derived && !meta["studio.history.undo"].derived);
    assert!(meta["studio.motion.keyframe"].expansion && meta["studio.workspace.panel"].expansion);
    assert!(meta["studio.object.layer.blending_mode"].enumeration);
    // Menu-only nodes of STU-SHL-040 are typed.
    for id in ["studio.help.full_command_index", "studio.help.diagnostics", "studio.automate.command_api", "studio.automate.model_lane"] {
        assert!(bar.find(id).is_some_and(|n| n.menu_only), "{id} menu_only");
    }
}

/// STU-SHL-049: twelve closed clause kinds, closed value sets, typed refusals, bounded input.
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
    // The predicate: the first unmet clause, its code and generated text; the remedy table names ONE command.
    let state = MenuState { profile: Profile { containers: [menu_tree::ContainerKind::Artboard].into(), ..Profile::default() }, ..MenuState::default() };
    let unmet = menu_tree::evaluate(&menu_tree::parse_requires("container:artboard & container:timeline").unwrap(), &state).unwrap_err();
    assert_eq!((unmet.code, unmet.text.as_str()), (ReasonCode::NoSuchContainer, "needs a timeline container"));
    assert_eq!(menu_tree::remedy_of(&unmet.clause), Some("studio.motion.timeline_doc.new_timeline"));
    assert!(menu_tree::evaluate(&[], &state).is_ok());
}

/// B6 done_when, part 1 (AC-REACH-04): all nineteen titles open by pointer and by id; every node of each title's
/// popup is in the tree at every level whatever its state (STU-SHL-018), each with its availability record as node
/// properties (STU-SHL-019); greyed rows are disabled with the reason as value; the four shell titles are greyed and
/// open a line saying why.
#[test]
fn all_nineteen_titles_open_and_every_node_is_drawn_with_its_record() {
    let s = Scratch::new("menu-titles");
    let mut f = open(&s.0, 1920, 1080);
    let ops = f.viewer().run().ops().len();
    // By pointer: a click on FILE opens it; a click on EDIT switches; a click outside closes.
    let (x, y) = f.centre("menu.file").unwrap();
    assert_eq!(run(&mut f, &click(x, y)), Some(ok("frame.menu_open")));
    assert_eq!(f.menu_open(), ["file"]);
    assert_eq!(sem(&f, "studio.menu.file").role, Role::Menu);
    assert!(sem(&f, "menu.file").selected, "the open title is drawn selected");
    let (x, y) = f.centre("menu.edit").unwrap();
    run(&mut f, &[Step::Event(HostEvent::PointerMoved { x, y })]);
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
            // GO, RUN, EDITORS, OPERATOR: the shell's titles, greyed with their reason (titles included, STU-SHL-052).
            let t = sem(&f, &aid(title));
            assert!(t.disabled, "{title} greyed");
            let a = t.availability.clone().expect("title availability");
            assert_eq!((a.state.as_str(), a.reason_code.as_deref()), ("NOT_IN_THIS_DOCUMENT", Some("NOT_IMPLEMENTED_YET")));
            let empty = sem(&f, &format!("studio.menu.{title}.empty"));
            assert_eq!(empty.value.as_deref(), Some(node.availability.reason_line().as_str()));
            continue;
        }
        for c in &node.children {
            let n = sem(&f, &aid(&c.id));
            assert_eq!(n.role, Role::MenuItem, "{}", c.id);
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
        // Every row is laid out inside the window or kept with no box (scrolled), never dropped.
        let p = f.panel_of(&format!("studio.menu.{title}")).unwrap();
        assert!(checks::overlapping_controls(p.tree()).is_empty(), "{title}");
        let b = p.tree().get(p.tree().find(&format!("studio.menu.{title}")).unwrap()).bounds;
        assert!(b.w() >= 220.0 && b.w() <= 480.0 && b.y1 <= 1080.0, "{title}: {b:?}");
        let clipped = f.clipped();
        assert!(clipped.is_empty(), "{title}: {clipped:?}");
    }
    // Every level: a submenu opens its cascade by id and its children are drawn too.
    assert_eq!(invoke(&mut f, &aid("studio.object.layer")), ok("frame.menu_open"));
    assert_eq!(f.menu_open(), ["object", "studio.object.layer"]);
    assert_eq!(sem(&f, "studio.menu.object.layer").role, Role::Menu);
    assert!(nodes(&f).iter().any(|n| n.author_id == aid("studio.object.layer.duplicate_layer")));
    // Opening and browsing ran nothing.
    assert_eq!(f.viewer().run().ops().len(), ops);
    let st = f.menu_stats();
    assert!(st.opens >= 21 && st.runs == 0, "{st:?}");
}

/// B6 done_when, part 2: a greyed item shows its reason and runs nothing (STU-SHL-018, STU-SHL-019): the status bar
/// says it, the outcome is the reason code, the menu stays open; MENU-INV-4: the run path refuses the same record.
#[test]
fn a_greyed_item_answers_its_reason_and_runs_nothing() {
    let s = Scratch::new("menu-greyed");
    let mut f = open(&s.0, 1920, 1080);
    let ops = f.viewer().run().ops().len();
    invoke(&mut f, "menu.file");
    let save_as = aid("studio.document.save_as");
    let row = sem(&f, &save_as);
    assert!(row.disabled);
    let (x, y) = f.centre(&save_as).unwrap();
    assert_eq!(run(&mut f, &click(x, y)), Some(Outcome { command: "frame.menu_unavailable", code: "not_implemented_yet".into() }));
    assert_eq!(f.menu_open(), ["file"], "the menu stays open on a greyed row");
    let status = f.node("studio.status.command").unwrap();
    assert_eq!(status.label.as_deref(), Some("Save As…: not built in this host yet"));
    assert_eq!(status.value.as_deref(), Some("frame.menu_unavailable not_implemented_yet"));
    // Nothing to undo yet: Undo is NOT_IN_THIS_DOCUMENT DOC_FEATURE_ABSENT with the clause; Redo likewise (its remedy,
    // Undo, is not available, so it has no single action, STU-SHL-050).
    let undo = f.menu_bar().find("studio.history.undo").unwrap().availability.clone();
    assert_eq!((undo.state, undo.reason_code, undo.profile_clause.as_deref()), (AvailabilityState::NotInThisDocument, Some(ReasonCode::DocFeatureAbsent), Some("doc_feature:undo_step")));
    let mut bar = f.menu_bar().clone();
    let refused = run_leaf(&mut f, &bar, "studio.history.undo").unwrap_err();
    assert_eq!((refused.code.as_str(), refused.availability.as_ref().map(|a| a.state)), ("doc_feature_absent", Some(AvailabilityState::NotInThisDocument)));
    assert_eq!(run_leaf(&mut f, &bar, "studio.object.layer").unwrap_err().code, "menu_not_a_leaf");
    assert_eq!(run_leaf(&mut f, &bar, "studio.workspace.panel.layers").unwrap_err().code, "menu_host_leaf");
    bar.titles.clear();
    assert_eq!(run_leaf(&mut f, &bar, "studio.history.undo").unwrap_err().code, "menu_node_unknown");
    assert_eq!(f.viewer().run().ops().len(), ops, "greyed rows reach no command");
    assert_eq!(f.menu_stats().unavailable, 1);
}

/// B6 done_when, part 3: a remedy runs. STU-SHL-020's worked example on a raster document: MOTION > Keyframes is
/// INAPPLICABLE_HERE NO_SUCH_CONTAINER with DOCUMENT > Timelines > New Timeline as its remedy row; the remedy runs the
/// catalog command, the profile grows (STU-SHL-048, signature STU-SHL-054), Keyframes is then NOT_IMPLEMENTED_YET (its
/// registry is not recovered, STU-SHL-038) and MOTION > Tracks > Add Track runs; both are undo steps; save and reopen
/// keep the timeline.
#[test]
fn the_remedy_runs_grows_the_profile_and_survives_save_and_reopen() {
    let s = Scratch::new("menu-remedy");
    let mut f = open(&s.0, 1920, 1080);
    assert_eq!(f.menu_state().profile.signature(), ["artboard"]);
    assert_eq!(f.menu_state().profile.named_shape(), Some("raster"));
    let kf = f.menu_bar().find("studio.motion.keyframe").unwrap().availability.clone();
    assert_eq!((kf.state, kf.reason_code, kf.remedy_command_id.as_deref()), (AvailabilityState::InapplicableHere, Some(ReasonCode::NoSuchContainer), Some("studio.motion.timeline_doc.new_timeline")));
    let track = f.menu_bar().find("studio.motion.tracks.add_track").unwrap().availability.clone();
    assert_eq!(track.state, AvailabilityState::InapplicableHere);
    invoke(&mut f, "menu.motion");
    let remedy = menu_tree::remedy_author_id("studio.motion.keyframe");
    assert_eq!(sem(&f, &remedy).label.as_deref(), Some("-> New Timeline"));
    let undo0 = f.viewer().run().history()["undo"].as_array().unwrap().len();
    let (x, y) = f.centre(&remedy).unwrap();
    assert_eq!(run(&mut f, &click(x, y)), Some(ok("video.project_new_sequence")));
    assert!(f.menu_open().is_empty(), "a leaf that ran closes the menus");
    assert_eq!(f.menu_state().profile.signature(), ["artboard", "timeline"]);
    assert_eq!(f.menu_state().profile.named_shape(), Some("composition"));
    assert_eq!(f.menu_stats().remedies, 1);
    assert_eq!(f.menu_stats().profile_growth, 2, "open, then the timeline");
    let kf = f.menu_bar().find("studio.motion.keyframe").unwrap().availability.clone();
    assert_eq!((kf.state, kf.reason_code), (AvailabilityState::NotInThisDocument, Some(ReasonCode::NotImplementedYet)));
    assert!(f.menu_bar().find("studio.motion.tracks.add_track").unwrap().availability.is_available());
    assert_eq!(invoke(&mut f, &aid("studio.motion.tracks.add_track")), ok("video.project_add_tracks"));
    let history = f.viewer().run().history();
    assert_eq!(history["undo"].as_array().unwrap().len(), undo0 + 2, "one undo step per command: {history}");
    // Attribution: every step carries the viewer's own run context (the caller's), never a default actor.
    let actor = f.viewer().run().actor();
    for step in history["undo_steps"].as_array().unwrap().iter().skip(undo0) {
        assert_eq!(step["actor"]["account_id"].as_str(), Some(actor.account_id()), "{step}");
        assert_eq!(step["actor"]["session_id"].as_str(), Some(actor.session_id()), "{step}");
    }
    // Save and reopen: the timeline is in the document, so the reopened profile declares it at once.
    let out = s.0.join("remedy.handshake");
    f.edit(|v| v.save(&out, false).unwrap());
    let g = GuiFrame::open(&out, 1280, 720, Some(1000)).unwrap();
    assert_eq!(g.menu_state().profile.signature(), ["artboard", "timeline"]);
    assert!(g.menu_bar().find("studio.motion.tracks.add_track").unwrap().availability.is_available());
    // The profile never shrinks silently: undoing the timeline leaves `timeline` declared (STU-SHL-048).
    let mut h = f;
    for _ in 0..2 {
        h.edit(|v| {
            v.undo().unwrap();
        });
    }
    assert!(h.viewer().run().snapshot().unwrap().document().sequences.is_empty(), "undo removed the sequence");
    assert_eq!(h.menu_state().profile.signature(), ["artboard", "timeline"], "the profile keeps what it declared");
}

/// Items dispatch typed commands: SELECT > Navigate Selection > First Object (view state), then OBJECT > Layer >
/// Duplicate Layer, EDIT > Duplicate, OBJECT > Group and Ungroup, Hide Selection, Bring Forward: each reports its
/// catalog command id, each mutation is one named undo step, EDIT > Undo (its chord resolved at render) reverts, and
/// the result reaches save and export.
#[test]
fn leaves_run_catalog_commands_as_undo_steps_that_reach_save_and_export() {
    let s = Scratch::new("menu-run");
    let mut f = open(&s.0, 1920, 1080);
    let rows0 = f.rows().len();
    // Nothing selected: Duplicate Layer is INAPPLICABLE_HERE NEEDS_SELECTION with First Object as its remedy.
    let dup = f.menu_bar().find("studio.object.layer.duplicate_layer").unwrap().availability.clone();
    assert_eq!((dup.reason_code, dup.remedy_command_id.as_deref()), (Some(ReasonCode::NeedsSelection), Some("studio.select.navigate_selection.first_object")));
    let ungroup = f.menu_bar().find("studio.object.ungroup").unwrap().availability.clone();
    assert_eq!(ungroup.reason_code, Some(ReasonCode::WrongSelectionKind));
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
    // The top layer (Sun) is hidden already: Hide Selection on it changes nothing, so walk down to a visible one.
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
    // EDIT > Undo: available now, chord resolved at render from the active set.
    invoke(&mut f, "menu.edit");
    let undo = sem(&f, &aid("studio.history.undo"));
    assert!(!undo.disabled);
    let p = f.panel_of(&aid("studio.history.undo")).unwrap();
    let words: Vec<String> = p.tree().nodes.iter().filter_map(|n| match &n.paint {
        kumihimo_ui::tree::Paint::Label { text, .. } => Some(text.clone()),
        _ => None,
    }).collect();
    assert!(words.iter().any(|w| w == "Ctrl+Z"), "chord drawn: {words:?}");
    assert_eq!(invoke(&mut f, &aid("studio.history.undo")), ok("history.undo"));
    assert!(f.menu_bar().find("studio.history.redo").unwrap().availability.is_available());
    // Save and export: the result reaches both, and a reopen shows it.
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
    // First Object twice, below, above, two duplicates, group, ungroup, the walk down, hide, send backward, undo; the
    // refused "above" at the top is not a run.
    assert_eq!(f.menu_stats().runs, runs + 3);
    assert_eq!(f.menu_stats().refused, 1);
}

/// STU-SHL-014: the Layers context menu (right click on a row, or by id as Shift+F10 would) lists references to
/// menu-bar leaves only, with the same author ids and the same availability, and runs the identical command.
#[test]
fn the_layers_context_menu_references_menu_bar_leaves() {
    let s = Scratch::new("menu-ctx");
    let mut f = open(&s.0, 1920, 1080);
    let red = f.rows().iter().find(|r| r.name == "Red").map(|r| r.layer_id.clone()).unwrap();
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
    // INAPPLICABLE_HERE rows are followed by their remedy rows (Ungroup: WRONG_SELECTION_KIND, remedy Group).
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
    // By id, as the Menu key on a focused row would.
    assert_eq!(f.open_context(&format!("{}.visibility", row_id(&red))), Ok(ok("frame.menu_open")));
    assert_eq!(f.open_context("studio.tab.layers"), Err("menu_context_none"));
    assert_eq!(f.open_context("studio.panel.layers.row.nope"), Err("unknown_layer"));
    let (x, y) = f.centre(gui::VIEWER_ID).unwrap();
    assert_eq!(run(&mut f, &click(x, y)), Some(ok("frame.menu_close")));
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
    let list: Vec<String> = sem(&f, "studio.menu.more").children.iter().map(|c| c.author_id.clone()).collect();
    let want: Vec<String> = hidden.iter().map(|h| gui::more_row_id(h.strip_prefix("menu.").unwrap())).collect();
    assert_eq!(list, want);
    let last = hidden.last().unwrap().strip_prefix("menu.").unwrap().to_owned();
    assert_eq!(invoke(&mut f, &gui::more_row_id(&last)), ok("frame.menu_open"));
    assert_eq!(f.menu_open(), [gui::MORE_MENU.to_owned(), last.clone()]);
    assert!(nodes(&f).iter().any(|n| n.author_id == format!("studio.menu.{last}")));
    assert_eq!(invoke(&mut f, gui::chrome::MORE_ID), ok("frame.menu_close"));
    assert_eq!(invoke(&mut f, &aid(&last)), ok("frame.menu_open"));
    let b = f.panel_of(&format!("studio.menu.{last}")).unwrap();
    let r = b.tree().get(b.tree().find(&format!("studio.menu.{last}")).unwrap()).bounds;
    assert!(r.x0 >= 0.0 && r.x1 <= 640.0 && r.y1 <= 480.0, "inside the window: {r:?}");
}

/// components.json menu_popup keyboard: Down and Up move through every row (greyed rows too), Enter opens a submenu
/// or runs a leaf, Escape closes the topmost popup; a long menu scrolls (rows kept in the tree with no box).
#[test]
fn keyboard_moves_opens_and_escapes() {
    let s = Scratch::new("menu-keys");
    let mut f = open(&s.0, 1024, 480);
    invoke(&mut f, "menu.file");
    let key = |f: &mut GuiFrame, k: Key| run(f, &chord(k, Default::default()));
    key(&mut f, Key::Down);
    assert!(sem(&f, &aid("studio.document.new")).selected, "the first row is highlighted");
    assert_eq!(key(&mut f, Key::Enter), Some(ok("frame.menu_open")));
    assert_eq!(f.menu_open(), ["file", "studio.document.new"]);
    key(&mut f, Key::Escape);
    assert_eq!(f.menu_open(), ["file"]);
    assert!(sem(&f, &aid("studio.document.new")).selected, "the closed submenu's row keeps the highlight");
    for _ in 0..3 {
        key(&mut f, Key::Up);
    }
    // FILE has more rows than fit at 480: the highlight scrolls into view; rows above it keep their nodes.
    let hot = nodes(&f).into_iter().find(|n| n.selected && n.role == Role::MenuItem && n.author_id.starts_with("menu.studio.")).unwrap();
    assert!(hot.bounds.h() > 0.0 && hot.bounds.y1 <= 480.0, "{hot:?}");
    assert!(nodes(&f).iter().any(|n| n.author_id == aid("studio.document.new") && n.bounds.h() == 0.0), "scrolled out, still in the tree");
    assert_eq!(key(&mut f, Key::Escape), Some(ok("frame.menu_close")));
    assert!(f.menu_open().is_empty());
}

/// STU-SHL-230 to 235 (AC-REACH-05): L0 after 400 ms on the hovered control (name and chord only), L1 after 900 ms;
/// L1 of a dimmed element shows the reason the node carries and the one-click remedy, which runs; suppressed while a
/// button is held, 500 ms after, on the control just operated, with tips off; no tip from script steps alone.
#[test]
fn tooltips_l0_and_l1_read_the_same_record() {
    let s = Scratch::new("menu-tips");
    let mut f = open(&s.0, 1920, 1080);
    // An agent-driven session: script steps alone never show a tip.
    let (x, y) = f.centre("menu.edit").unwrap();
    run(&mut f, &[Step::Event(HostEvent::PointerMoved { x, y })]);
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
    // A press hides it; nothing for 500 ms after the release; nothing on the control just operated.
    run(&mut f, &click(x, y));
    assert!(f.tip().is_none());
    f.advance(1000);
    assert!(f.tip().is_none(), "the control just operated");
    // The open EDIT menu: L1 of Redo (dimmed) through the accessibility request.
    let redo = aid("studio.history.redo");
    let r = f.show_tip(&redo, 1).unwrap();
    f.frame().unwrap();
    let node = sem(&f, &redo);
    let a = node.availability.clone().unwrap();
    assert_eq!(r.reason, a.reason_text, "the tip shows the reason the node carries");
    assert_eq!(r.value, node.value, "the tip and the node carry the same reason line");
    assert_eq!(sem(&f, gui::chrome::TIP_ID).value, node.value);
    // L0 of Undo: name and the resolved chord, no reason.
    let l0 = f.show_tip(&aid("studio.history.undo"), 0).unwrap();
    assert_eq!((l0.name.as_str(), l0.chord.as_deref(), l0.reason.as_deref()), ("Undo", Some("Ctrl+Z"), None));
    // L1 of Keyframes: reason and remedy; the remedy button runs it.
    invoke(&mut f, "menu.motion");
    let kf = aid("studio.motion.keyframe");
    let t = f.show_tip(&kf, 1).unwrap();
    assert_eq!((t.reason.as_deref(), t.remedy.as_deref(), t.remedy_label.as_deref()), (Some("needs a timeline container"), Some("studio.motion.timeline_doc.new_timeline"), Some("New Timeline")));
    f.frame().unwrap();
    assert_eq!(sem(&f, gui::chrome::TIP_REMEDY_ID).role, Role::Button);
    assert_eq!(invoke(&mut f, gui::chrome::TIP_REMEDY_ID), ok("video.project_new_sequence"));
    assert!(f.tip().is_none() && f.menu_state().profile.signature().contains(&"timeline"));
    // Tips off (STU-SHL-235 rule 4).
    f.set_tips(true, None);
    let (x, y) = f.centre("menu.view").unwrap();
    run(&mut f, &[Step::Event(HostEvent::PointerMoved { x, y })]);
    f.advance(2000);
    assert!(f.tip().is_none());
    assert_eq!(f.show_tip("studio.bogus.control", 1), Err("unknown_author_id"));
    assert!(f.menu_stats().tips >= 5);
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
    for want in [r#""menu":{"open":["file"]"#, r#""profile_signature":["artboard"]"#, r#""named_shape":"raster""#, r#""nodes":"#, r#""evaluations":"#] {
        assert!(receipt.contains(want), "{want} in {receipt}");
    }
    let json = a.menu_json();
    assert!(json.contains(r#""schema":"hsk.studio.controls.menu@1""#) && json.contains(r#""named_shape":"raster""#), "{}", &json[..200.min(json.len())]);
    // The tree file carries the availability record as node properties (STU-SHL-019) for the open popup.
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
    run(&mut f, &chord(Key::Escape, Default::default()));
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

/// The Vulkan window path (GPU group): an open cascade and an L1 tip drawn by the UI pass match the CPU painter outside
/// the antialiased rects; validation clean.
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
    let mut cpu = CpuChrome::default();
    f.compose(&mut cpu).unwrap();
    let want = cpu.canvas.unwrap().rgba().to_vec();
    let (max, n, outside) = gui::parity(&want, &got, w, pass.trace());
    println!("menu parity: max {max} differing {n} outside_aa {outside}");
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
