//! Lane SMB-HARNESS-SHELL-2: menu and window descriptors ([STU-ARC-014]) and the menu law they
//! can check ([STU-SHL-008] / [STU-SHL-017] / [STU-SHL-018] / [STU-SHL-019] / [STU-SHL-020] /
//! [STU-SHL-021] / [STU-SHL-050] / [STU-SHL-051] / [STU-SHL-251]). Headless: one bare
//! `egui::Context`, pointer events only, no window.

use egui::{Event, Modifiers, PointerButton, Pos2, RawInput, Rect, vec2};
use hsk_studio_controls::menu::{
    Availability, AvailabilityState, MenuBar, MenuNode, MenuUi, ReasonCode, WindowRole, WindowSpec, menu_bar_ui, remedy_author_id,
    vendor_product_names,
};

fn id_of(name: &str) -> egui::Id {
    egui::Id::new(("menu-test", name))
}

fn codes(bar: &MenuBar) -> Vec<(&'static str, String)> {
    let mut v: Vec<(&'static str, String)> = bar.audit().into_iter().map(|i| (i.code, i.node)).collect();
    v.sort();
    v
}

/// A clean bar: File (Open, Save greyed with Open as remedy), Help (Manual submenu with one
/// unimplemented leaf).
fn sample() -> MenuBar {
    MenuBar::new(vec![
        MenuNode::title(
            "file",
            "File",
            vec![
                MenuNode::item("studio.document.open", "Open…").with_shortcut("Ctrl+O").with_help("Opens a file."),
                MenuNode::item("studio.document.save", "Save")
                    .with_shortcut("Ctrl+S")
                    .with_availability(Availability::inapplicable(ReasonCode::NoSuchContainer, "nothing open to save", "studio.document.open")),
            ],
        ),
        MenuNode::title(
            "help",
            "Help",
            vec![MenuNode::submenu(
                "studio.help.manual",
                "Studio Manual",
                vec![MenuNode::item("studio.help.manual.this_tool", "Help for This Tool").with_availability(Availability::not_in_document(
                    ReasonCode::NotImplementedYet,
                    "not yet",
                    None,
                ))],
            )],
        ),
    ])
}

#[test]
fn vendor_names_are_found_as_tokens_never_as_substrings() {
    // Positives: case, camel case, separators, two-token names, ids.
    for (text, name) in [
        ("Figma (.fig)", "figma"),
        ("PHOTOSHOP", "photoshop"),
        ("After Effects", "aftereffects"),
        ("AfterEffects", "aftereffects"),
        ("after_effects", "aftereffects"),
        ("InDesign", "indesign"),
        ("In Design", "indesign"),
        ("lightroom_classic", "lightroom"),
        ("studio.workspace.panel.figma_player", "figma"),
        ("Fig Jam board", "figjam"),
        ("Photo Shop", "photoshop"),
        ("  affinity\tdesigner ", "affinity"),
    ] {
        assert!(vendor_product_names(text).contains(&name), "{text:?} should name {name}");
    }
    // Decoys: neutral words, prefixes and words that merely contain a vendor name.
    for text in ["Design file (.fig)", "Photo", "Designer", "Publisher", "Open Prototype", "figure", "premier", "indesignable", "photoshopped", "Effects", "after", "", "studio.document.open"] {
        assert!(vendor_product_names(text).is_empty(), "{text:?} is not a vendor name: {:?}", vendor_product_names(text));
    }
}

#[test]
fn audit_is_clean_on_a_lawful_bar_and_names_every_breach() {
    assert_eq!(codes(&sample()), vec![]);
    let deep = MenuNode::submenu(
        "a.one",
        "One",
        vec![MenuNode::submenu("a.two", "Two", vec![MenuNode::submenu("a.three", "Three", vec![MenuNode::submenu("a.four", "Four", vec![MenuNode::item("a.five", "Five")])])])],
    );
    let bad = MenuBar::new(vec![
        MenuNode::title(
            "file",
            "File",
            vec![
                MenuNode::item("studio.open", "Figma (.fig)"),
                MenuNode::item("studio.open", "Again"),
                MenuNode::item("Bad Id", "x"),
                MenuNode::item("studio.blank", "  "),
                MenuNode::item("studio.photoshop_import", "Import"),
                MenuNode::item("studio.no_reason", "No reason").with_availability(Availability {
                    state: AvailabilityState::NotInThisDocument,
                    reason_code: None,
                    reason_text: None,
                    remedy_command_id: None,
                    profile_clause: None,
                }),
                MenuNode::item("studio.no_remedy", "No remedy").with_availability(Availability {
                    state: AvailabilityState::InapplicableHere,
                    reason_code: Some(ReasonCode::NeedsSelection),
                    reason_text: Some("select".into()),
                    remedy_command_id: None,
                    profile_clause: None,
                }),
                MenuNode::item("studio.unknown_remedy", "Unknown remedy").with_availability(Availability::inapplicable(ReasonCode::NeedsSelection, "select", "studio.nowhere")),
                MenuNode::item("studio.greyed_remedy", "Greyed remedy").with_availability(Availability::inapplicable(ReasonCode::NeedsSelection, "select", "studio.no_reason")),
                MenuNode::submenu("studio.empty", "Empty", vec![]),
                MenuNode::title("studio.nested_title", "Nested", vec![MenuNode::item("studio.nested_leaf", "Leaf")]),
                deep,
                MenuNode::item("studio.recent.0", "C:/work/figma-export.psd").data_label(),
            ],
        ),
        MenuNode::item("studio.loose", "Loose"),
    ]);
    let found = codes(&bad);
    let has = |code: &str, node: &str| found.iter().any(|(c, n)| *c == code && n == node);
    assert!(has("vendor_product_name_in_display_name", "studio.open"), "{found:?}");
    assert!(has("vendor_product_name_in_display_name", "studio.photoshop_import"), "id is checked too: {found:?}");
    assert!(has("menu_id_duplicate", "studio.open"));
    assert!(has("descriptor_id_invalid", "Bad Id"));
    assert!(has("descriptor_display_name_empty", "studio.blank"));
    assert!(has("availability_reason_missing", "studio.no_reason"));
    assert!(has("inapplicable_without_remedy", "studio.no_remedy"));
    assert!(has("remedy_unknown", "studio.unknown_remedy"));
    assert!(has("remedy_not_available", "studio.greyed_remedy"));
    assert!(has("menu_submenu_empty", "studio.empty"));
    assert!(has("menu_title_nested", "studio.nested_title"));
    assert!(has("menu_depth_exceeded", "a.five"), "depth 5 below the title (STU-SHL-021): {found:?}");
    assert!(!has("menu_depth_exceeded", "a.four"));
    assert!(has("menu_top_level_not_title", "studio.loose"));
    // An operator file path is data: its label is not a Studio name.
    assert!(!found.iter().any(|(_, n)| n == "studio.recent.0"), "{found:?}");
    // Windows: the same naming checks.
    let w = WindowSpec::new("dialog", "Open Figma", "x").with("dialog.ok", "OK", WindowRole::Action, "").with("dialog.ok", "", WindowRole::Field, "");
    let wc: Vec<&str> = w.audit().iter().map(|i| i.code).collect();
    assert!(wc.contains(&"vendor_product_name_in_display_name") && wc.contains(&"menu_id_duplicate") && wc.contains(&"descriptor_display_name_empty"), "{wc:?}");
    assert!(WindowSpec::new("dialog", "Open", "x").with("dialog.ok", "OK", WindowRole::Action, "").audit().is_empty());
}

#[test]
fn remedies_resolve_to_one_available_leaf_or_the_node_leaves_the_inapplicable_state() {
    let mut bar = MenuBar::new(vec![MenuNode::title(
        "panels",
        "Panels",
        vec![
            MenuNode::item("p.open", "Open…"),
            MenuNode::item("p.wave", "Waveform").with_availability(Availability::inapplicable(ReasonCode::DocFeatureAbsent, "open audio", "p.open")),
            MenuNode::item("p.spectral", "Spectral").with_availability(Availability::inapplicable(ReasonCode::DocFeatureAbsent, "open the waveform", "p.wave")),
            MenuNode::item("p.blocked", "Blocked").with_availability(Availability::not_in_document(ReasonCode::NotImplementedYet, "later", None)),
            MenuNode::item("p.orphan", "Orphan").with_availability(Availability::inapplicable(ReasonCode::DocFeatureAbsent, "needs blocked", "p.blocked")),
        ],
    )]);
    bar.resolve_remedies();
    let a = |id: &str| bar.find(id).unwrap().availability.clone();
    assert_eq!(a("p.spectral").remedy_command_id.as_deref(), Some("p.open"), "the remedy of a greyed remedy is followed");
    assert_eq!(a("p.spectral").state, AvailabilityState::InapplicableHere);
    assert_eq!(a("p.orphan").state, AvailabilityState::NotInThisDocument);
    assert_eq!((a("p.orphan").reason_code, a("p.orphan").remedy_command_id), (Some(ReasonCode::DocFeatureAbsent), None));
    assert_eq!(codes(&bar), vec![]);
}

#[test]
fn every_projection_reads_the_one_availability_record() {
    let bar = sample();
    let save = bar.find("studio.document.save").unwrap();
    let line = save.availability.reason_line();
    assert_eq!(line, "INAPPLICABLE_HERE NO_SUCH_CONTAINER: nothing open to save; remedy studio.document.open");
    assert_eq!(bar.find("studio.document.open").unwrap().availability.reason_line(), "");
    // Agent JSON.
    let j = bar.agent_json();
    assert_eq!(j["schema"], "hsk.studio.controls.menu@1");
    let node = j["nodes"].as_array().unwrap().iter().find(|n| n["id"] == "studio.document.save").unwrap().clone();
    assert_eq!(node["author_id"], "menu.studio.document.save");
    assert_eq!(node["menu_path"], serde_json::json!(["File", "Save"]));
    assert_eq!(node["availability"]["availability_state"], "INAPPLICABLE_HERE");
    assert_eq!(node["availability"]["reason_code"], "NO_SUCH_CONTAINER");
    assert_eq!(node["availability"]["remedy_command_id"], "studio.document.open");
    assert_eq!(node["shortcut"], "Ctrl+S");
    // Manual topics carry the same reason line.
    let topics = bar.manual_topics();
    let t = topics.iter().find(|t| t.id == "menu.studio.document.save").unwrap();
    assert!(t.body.contains(&line) && t.body.contains("File > Save") && t.body.contains("Ctrl+S"), "{}", t.body);
    // The closed sets ([STU-SHL-050] / [STU-SHL-051]).
    assert_eq!(ReasonCode::ALL.len(), 12);
    let names: Vec<&str> = ReasonCode::ALL.iter().map(|c| c.code()).collect();
    assert!(names.contains(&"NOT_IMPLEMENTED_YET") && names.contains(&"NO_SUCH_CONTAINER"));
}

// ---- headless egui ---------------------------------------------------------------------------

fn run(ctx: &egui::Context, bar: &MenuBar, events: Vec<Event>) -> MenuUi {
    let input = RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1000.0, 700.0))), events, ..Default::default() };
    let mut out = MenuUi::default();
    let _ = ctx.run(input, |ctx| {
        egui::TopBottomPanel::top(id_of("bar")).show(ctx, |ui| {
            out = menu_bar_ui(ui, bar, &id_of);
        });
    });
    out
}

fn click(ctx: &egui::Context, bar: &MenuBar, at: Pos2) -> Vec<MenuUi> {
    let button = |pressed| Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
    vec![run(ctx, bar, vec![Event::PointerMoved(at), button(true)]), run(ctx, bar, vec![button(false)]), run(ctx, bar, vec![])]
}

fn node<'a>(ui: &'a MenuUi, author: &str) -> &'a hsk_studio_controls::menu::RenderedNode {
    ui.nodes.iter().find(|n| n.author_id == author).unwrap_or_else(|| panic!("{author} not drawn: {:?}", ui.drawn()))
}

#[test]
fn menu_bar_ui_never_hides_a_node_and_dispatches_the_remedy_leaf() {
    let bar = sample();
    let ctx = egui::Context::default();
    let first = run(&ctx, &bar, vec![]);
    assert_eq!(first.drawn(), ["menu.file", "menu.help"], "closed menus draw their titles");
    assert!(first.clicked.is_none());
    // Open File: the greyed Save leaf is drawn disabled with its reason, its remedy beside it.
    let file = node(&first, "menu.file").rect.center();
    let opened = click(&ctx, &bar, file).pop().unwrap();
    let save = node(&opened, "menu.studio.document.save");
    assert!(!save.enabled);
    assert_eq!(save.reason, bar.find("studio.document.save").unwrap().availability.reason_line());
    assert!(node(&opened, "menu.studio.document.open").enabled);
    let remedy = node(&opened, &remedy_author_id("studio.document.save"));
    assert!(remedy.enabled);
    let (save_at, remedy_at) = (save.rect.center(), remedy.rect.center());
    // Clicking the remedy dispatches Open (and closes the menu).
    let clicked: Vec<Option<String>> = click(&ctx, &bar, remedy_at).into_iter().map(|u| u.clicked).collect();
    assert!(clicked.contains(&Some("studio.document.open".to_owned())), "{clicked:?}");
    // Reopened, a click on the disabled leaf dispatches nothing.
    let reopened = click(&ctx, &bar, file).pop().unwrap();
    assert!(!node(&reopened, "menu.studio.document.save").enabled);
    let clicked: Vec<Option<String>> = click(&ctx, &bar, save_at).into_iter().map(|u| u.clicked).collect();
    assert!(clicked.iter().all(Option::is_none), "{clicked:?}");
}
