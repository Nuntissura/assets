//! Lane SMB-HARNESS-SHELL-1 (feature `viewer`): the desktop shell driven headlessly (no window,
//! CX-SAFE-002): menu bar FILE / EDIT / VIEW / WORKSPACE, the panel function keys, the in-crate file
//! dialog, the recent-files list and the hosted surfaces. Every surface is opened both through its
//! function key (a real egui key event in one UI frame) and through `Viewer::shell_command`, the
//! exact call a menu click makes; with feature `desktop` the File menu and its Open entry are
//! clicked through AccessKit action requests (egui's own input path), and greyed entries are
//! checked as disabled AccessKit nodes that name their feature. Oracles: the viewer's own state
//! projection (`state_json()["shell"]`), the widget names it drew, files written to a scratch dir.
#![cfg(feature = "viewer")]

mod common;
mod support_audio;

use common::{Scratch, synthetic_dng, two_layer_psd};
use hsk_studio_accord::{CancellationToken, Ticks};
use hsk_studio_folio::{self as folio, *};
use hsk_studio_harness::layout_ui::LayoutEdit;
use hsk_studio_harness::view::Viewer;
use hsk_studio_harness::view::file_dialog::{self, FileDialog, Purpose, list_dir, parse_typed};
use hsk_studio_harness::view::render::Prefer;
use hsk_studio_controls::menu::{AvailabilityState, ReasonCode, vendor_product_names};
use hsk_studio_harness::view::shell::{
    self, Availability, CompiledFeatures, HelpCommand, HelpPage, Panel, ShellCommand, decode_recent, encode_recent, entry_label, feature_gate, menu_entries,
};
use serde_json::Value;
use std::path::{Path, PathBuf};

// ---- helpers ---------------------------------------------------------------------------------------

fn input(events: Vec<egui::Event>) -> egui::RawInput {
    egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1400.0, 900.0))), events, ..Default::default() }
}

fn frame(v: &mut Viewer, ctx: &egui::Context, events: Vec<egui::Event>) -> egui::FullOutput {
    ctx.run(input(events), |ctx| v.ui(ctx))
}

fn key(k: egui::Key, modifiers: egui::Modifiers) -> egui::Event {
    egui::Event::Key { key: k, physical_key: None, pressed: true, repeat: false, modifiers }
}

fn press(v: &mut Viewer, ctx: &egui::Context, k: egui::Key, modifiers: egui::Modifiers) {
    let _ = frame(v, ctx, vec![key(k, modifiers)]);
}

fn drew(v: &Viewer, name: &str) -> bool {
    v.drawn().iter().any(|d| d == name)
}

fn panel(v: &Viewer, p: Panel) -> Value {
    v.state_json()["shell"]["panels"][p.key()].clone()
}

fn last_status(v: &Viewer) -> String {
    v.state_json()["status"].as_array().and_then(|a| a.last()).and_then(Value::as_str).unwrap_or_default().to_owned()
}

/// A native package whose document has one sequence (`seq-main`, one audio track with one clip).
fn sequence_package(dir: &Path) -> PathBuf {
    const F: u64 = 10_160_640_000;
    let clip = StudioClip {
        schema_id: ClipSchema::V1,
        clip_id: "SCLP-019abcde-0000-7000-8000-000000000001".into(),
        source_ref: ClipSource::Media { artifact_manifest_id: "tone".into(), content_digest: ContentDigest { algorithm: "sha256".into(), digest: "abc123".into() }, asset_id: None },
        source_in: Ticks::new(0),
        source_out: Ticks::new(25 * F),
        timeline_start: Ticks::new(0),
        duration: Ticks::new(25 * F),
        speed: Speed::UNIT,
        enabled: true,
        link_group: None,
        markers: Vec::new(),
        transitions: ClipTransitions::default(),
    };
    let track = StudioTrack {
        schema_id: TrackSchema::V1,
        track_id: "STRK-019abcde-0000-7000-8000-000000000001".into(),
        kind: TrackKind::Audio,
        index: 0,
        name: "A1".into(),
        enabled: true,
        locked: false,
        sync_locked: true,
        targeted: true,
        muted: false,
        soloed: false,
        clips: vec![clip],
        volume: 1.0,
        pan: 0.0,
    };
    let sequence = StudioSequence {
        schema_id: SequenceSchema::V1,
        sequence_id: "seq-main".into(),
        name: "main".into(),
        frame_rate: SequenceFrameRate { ticks_per_frame: F },
        duration: Ticks::new(25 * F),
        tracks: vec![track],
        markers: Vec::new(),
        work_area: None,
    };
    let doc = serde_json::json!({
        "schema_id":"hsk.studio.document@1","document_id":"SDOC-019abcde-0000-7000-8000-000000000003","revision":1,"geometry_unit":"px","colour_mode":3,"bits_per_channel":8,
        "working_space_rgb":null,"working_space_cmyk":null,"working_space_gray":null,"working_space_spot":null,"blending_space":null,
        "artboards":[],"page_spreads":[],
        "layers":[{"schema_id":"hsk.studio.layer@1","layer_id":"SLYR-019abcde-0000-7000-8000-000000000001","name":"group","parent":null,"kind":"group","payload":{"payload_kind":"group"},"visible":true}],
        "graph":{"schema_id":"hsk.studio.layer_graph@1","operations":[],"edges":[],"outputs":[]},
        "sequences":[serde_json::to_value(&sequence).unwrap()]
    });
    struct Any;
    impl folio::Resolver for Any {
        fn primitive(&self, _: &str, _: &str) -> Resolution {
            Resolution::Available
        }
        fn profile(&self, _: &str) -> Resolution {
            Resolution::Available
        }
        fn tile(&self, _: &TileRef) -> Resolution {
            Resolution::Available
        }
        fn format_supported(&self, _: &str) -> bool {
            true
        }
        fn primitive_layer_binding(&self, _: &str, _: &str) -> bool {
            false
        }
        fn media(&self, _: &str, _: &ContentDigest) -> Resolution {
            Resolution::Available
        }
    }
    let token = CancellationToken::default();
    let snapshot = match folio::inspect_bytes(&serde_json::to_vec(&doc).unwrap(), Budget::default(), &token, &Any).unwrap() {
        Inspection::Editable(s) => s,
        Inspection::ReadOnly(_) => panic!("read-only fixture"),
    };
    let assets = hsk_studio_package::MemoryAssetSource::new();
    let mut bytes = Vec::new();
    hsk_studio_package::write_package_with(
        &snapshot,
        &assets as &dyn hsk_studio_package::AssetSource,
        &hsk_studio_package::Extras { quarantined: &[], previews: &[] },
        &hsk_studio_package::WriteOptions::default(),
        &hsk_studio_package::Limits::default(),
        &token,
        &mut bytes,
    )
    .unwrap();
    let path = dir.join("sequence.handshake");
    std::fs::write(&path, bytes).unwrap();
    path
}

fn psd_at(dir: &Path, name: &str) -> PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, two_layer_psd()).unwrap();
    p
}

fn abs(p: &Path) -> PathBuf {
    std::path::absolute(p).unwrap()
}

#[cfg(feature = "desktop")]
fn ak_find(out: &egui::FullOutput, pred: impl Fn(&str) -> bool) -> Option<(egui::accesskit::NodeId, bool)> {
    let tree = out.platform_output.accesskit_update.as_ref()?;
    tree.nodes.iter().find(|(_, n)| n.label().is_some_and(&pred)).map(|(id, n)| (*id, n.is_disabled()))
}

#[cfg(feature = "desktop")]
fn ak_click(id: egui::accesskit::NodeId) -> egui::Event {
    egui::Event::AccessKitActionRequest(egui::accesskit::ActionRequest { action: egui::accesskit::Action::Click, target: id, data: None })
}

// ---- menu model ------------------------------------------------------------------------------------

#[test]
fn menu_bar_draws_its_titles_and_every_entry_has_a_unique_command_id() {
    let entries = menu_entries();
    let mut ids: Vec<String> = entries.iter().map(|e| e.command.id()).collect();
    let n = ids.len();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), n, "duplicate command ids: {ids:?}");
    for p in Panel::ALL {
        let e = entries.iter().find(|e| e.command == ShellCommand::Panel(p)).unwrap_or_else(|| panic!("{p:?} has no WORKSPACE entry"));
        assert_eq!((e.menu, e.shortcut), ("Workspace", Some(p.shortcut_text())));
    }
    for (cmd, short) in [(ShellCommand::Open, "Ctrl+O"), (ShellCommand::Save, "Ctrl+S"), (ShellCommand::SaveAs, "Ctrl+Shift+S"), (ShellCommand::Close, "Ctrl+W"), (ShellCommand::Undo, "Ctrl+Z"), (ShellCommand::Redo, "Ctrl+Y")] {
        assert_eq!(entries.iter().find(|e| e.command == cmd).unwrap().shortcut, Some(short));
    }
    let mut v = Viewer::new(Prefer::Cpu);
    let ctx = egui::Context::default();
    let _ = frame(&mut v, &ctx, vec![]);
    for name in [shell::widget::MENU_BAR, shell::widget::FILE, shell::widget::EDIT, shell::widget::VIEW, shell::widget::WORKSPACE, shell::widget::HELP] {
        assert!(drew(&v, name), "{name} not drawn: {:?}", v.drawn());
    }
    // Drawing committed nothing.
    assert!(v.run().ops().is_empty());
}

/// Feature `desktop`: File and File > Open… clicked through AccessKit open the file dialog; the
/// WORKSPACE > Layout Editor entry clicked the same way hosts the layout editor; an entry whose
/// feature is missing is a disabled node naming the feature.
#[cfg(feature = "desktop")]
#[test]
fn accesskit_clicks_reach_the_open_dialog_and_a_workspace_panel() {
    let mut v = Viewer::new(Prefer::Cpu);
    let ctx = egui::Context::default();
    ctx.enable_accesskit();
    let out = frame(&mut v, &ctx, vec![]);
    let (file, disabled) = ak_find(&out, |l| l == "File").expect("File menu node");
    assert!(!disabled);
    let _ = frame(&mut v, &ctx, vec![ak_click(file)]);
    let out = frame(&mut v, &ctx, vec![]);
    assert!(drew(&v, &shell::widget::entry("studio.document.open")), "{:?}", v.drawn());
    let (open, disabled) = ak_find(&out, |l| l.starts_with("Open…")).expect("Open… entry node");
    assert!(!disabled);
    let _ = frame(&mut v, &ctx, vec![ak_click(open)]);
    let _ = frame(&mut v, &ctx, vec![]);
    let s = v.state_json();
    assert_eq!(s["shell"]["dialog"]["purpose"], "open", "{}", s["shell"]);
    assert!(drew(&v, file_dialog::widget::WINDOW) && drew(&v, file_dialog::widget::PATH), "{:?}", v.drawn());
    v.dialog_cancel();

    let out = frame(&mut v, &ctx, vec![]);
    let (workspace, _) = ak_find(&out, |l| l == "Workspace").expect("Workspace menu node");
    let _ = frame(&mut v, &ctx, vec![ak_click(workspace)]);
    let out = frame(&mut v, &ctx, vec![]);
    // WORKSPACE > Panels ([STU-SHL-036] expansion node).
    let (panels, disabled) = ak_find(&out, |l| l == "Panels").expect("Panels submenu node");
    assert!(!disabled);
    let _ = frame(&mut v, &ctx, vec![ak_click(panels)]);
    let out = frame(&mut v, &ctx, vec![]);
    assert!(drew(&v, shell::widget::PANELS), "{:?}", v.drawn());
    // Greyed entries are disabled nodes whose label names the feature.
    for p in Panel::ALL {
        if let Availability::NeedsFeature(f) = feature_gate(p, &CompiledFeatures::current()) {
            let (_, disabled) = ak_find(&out, |l| l.starts_with(&entry_label(p, &Availability::NeedsFeature(f)))).unwrap_or_else(|| panic!("{p:?} entry node"));
            assert!(disabled, "{p:?} needs {f} but is enabled");
        }
    }
    let (layout, disabled) = ak_find(&out, |l| l.starts_with("Layout Editor")).expect("Layout Editor entry node");
    assert!(!disabled);
    let _ = frame(&mut v, &ctx, vec![ak_click(layout)]);
    let _ = frame(&mut v, &ctx, vec![]);
    assert_eq!(panel(&v, Panel::LayoutEditor)["visible"], true, "{}", v.state_json()["shell"]);
    assert!(drew(&v, "layout"), "{:?}", v.drawn());
}

// ---- surfaces --------------------------------------------------------------------------------------

#[test]
fn function_keys_and_workspace_entries_open_every_surface() {
    let scratch = Scratch::new("shell-surfaces");
    let mut v = Viewer::new(Prefer::Cpu);
    let ctx = egui::Context::default();
    v.open(&sequence_package(&scratch.0)).unwrap_or_else(|e| panic!("{e}"));
    let none = egui::Modifiers::NONE;

    // Layers and Properties: shown by default; their keys hide and show them again.
    let _ = frame(&mut v, &ctx, vec![]);
    assert!(drew(&v, "layers") && drew(&v, "properties"));
    for (p, k, id) in [(Panel::Layers, egui::Key::F2, "layers"), (Panel::Properties, egui::Key::F3, "properties")] {
        press(&mut v, &ctx, k, none);
        assert_eq!(panel(&v, p)["visible"], false);
        let _ = frame(&mut v, &ctx, vec![]);
        assert!(!drew(&v, id), "{id} still drawn");
        v.shell_command(ShellCommand::Panel(p)).unwrap();
        let _ = frame(&mut v, &ctx, vec![]);
        assert!(drew(&v, id), "{id} not drawn again");
    }

    // Develop needs a camera raw: refused with the reason, nothing opens.
    assert_eq!(panel(&v, Panel::Develop)["availability"], "unavailable");
    press(&mut v, &ctx, egui::Key::F4, none);
    assert!(last_status(&v).contains("camera raw"), "{}", last_status(&v));
    assert_eq!(panel(&v, Panel::Develop)["open"], false);

    // Layout editor (F5): the document has no pages, so a blank one-page layout document is hosted.
    press(&mut v, &ctx, egui::Key::F5, none);
    let _ = frame(&mut v, &ctx, vec![]);
    assert_eq!(panel(&v, Panel::LayoutEditor)["visible"], true);
    assert!(drew(&v, "layout") && drew(&v, "layout.pages"), "{:?}", v.drawn());
    let ed = v.layout_editor().unwrap();
    ed.apply(LayoutEdit::DrawFrame { page_id: "page-1".into(), x: 36.0, y: 36.0, w: 200.0, h: 100.0 }).expect("a text frame on the blank page");
    assert_eq!(ed.history_len(), (1, 0));
    press(&mut v, &ctx, egui::Key::F5, none);
    let _ = frame(&mut v, &ctx, vec![]);
    assert!(!drew(&v, "layout"));
    assert_eq!(v.layout_editor().unwrap().history_len(), (1, 0), "hiding keeps the editor");

    // Program monitor (F6) on the document's sequence.
    press(&mut v, &ctx, egui::Key::F6, none);
    let _ = frame(&mut v, &ctx, vec![]);
    assert_eq!(panel(&v, Panel::ProgramMonitor)["visible"], true, "{}", v.state_json()["shell"]);
    assert!(drew(&v, "monitor") && drew(&v, "monitor.play"), "{:?}", v.drawn());

    // Prototype player (F12): a dialog filtered to .fig, then the play window.
    let fig = scratch.0.join("proto.fig");
    std::fs::copy(format!("{}/../hsk-studio-interop-fig/tests/fixtures/fig-prototype-pages.fig", env!("CARGO_MANIFEST_DIR")), &fig).unwrap();
    press(&mut v, &ctx, egui::Key::F12, none);
    let s = v.state_json();
    assert_eq!(s["shell"]["dialog"]["purpose"], "open_prototype");
    assert_eq!(s["shell"]["dialog"]["filter"], "Design file (.fig)");
    assert!(s["shell"]["dialog"]["entries"].as_array().unwrap().iter().any(|e| e["name"] == "proto.fig"), "{}", s["shell"]["dialog"]);
    assert!(!s["shell"]["dialog"]["entries"].as_array().unwrap().iter().any(|e| e["name"] == "sequence.handshake"));
    v.file_dialog().unwrap().typed = "proto.fig".into();
    assert_eq!(v.dialog_accept(), Ok(true));
    let _ = frame(&mut v, &ctx, vec![]);
    assert_eq!(panel(&v, Panel::PrototypePlayer)["visible"], true);
    assert!(drew(&v, hsk_studio_harness::view::prototype::widget::PANEL), "{:?}", v.drawn());
    assert!(v.state_json()["shell"]["dialog"].is_null());

    // Source monitor (F7): feature media.
    #[cfg(feature = "media")]
    {
        let media = scratch.0.join("clip.mov");
        std::fs::copy(format!("{}/../hsk-studio-reel/tests/fixtures/prores/prores-422hq-40x24-p.mov", env!("CARGO_MANIFEST_DIR")), &media).unwrap();
        press(&mut v, &ctx, egui::Key::F7, none);
        assert_eq!(v.state_json()["shell"]["dialog"]["purpose"], "open_source_media");
        v.file_dialog().unwrap().typed = media.to_string_lossy().into_owned();
        assert_eq!(v.dialog_accept(), Ok(true), "{}", v.state_json()["status"]);
        let _ = frame(&mut v, &ctx, vec![]);
        assert_eq!(panel(&v, Panel::SourceMonitor)["visible"], true);
        assert!(drew(&v, "source_monitor") && drew(&v, "source_monitor.mark_in"), "{:?}", v.drawn());
    }
    #[cfg(not(feature = "media"))]
    {
        assert_eq!(panel(&v, Panel::SourceMonitor)["availability"], "needs_feature");
        press(&mut v, &ctx, egui::Key::F7, none);
        assert!(last_status(&v).contains("needs feature media"), "{}", last_status(&v));
        assert!(v.state_json()["shell"]["dialog"].is_null());
    }

    // Waveform editor (F9), spectral display (F11), mixer (F8): feature audio-batch, an audio file open.
    #[cfg(feature = "audio-batch")]
    {
        let wav = scratch.0.join("tone.wav");
        let signal = support_audio::signal(4800, 2);
        std::fs::write(&wav, support_audio::wav(2, 48_000, support_audio::WavFormat::Float32, None, &signal)).unwrap();
        assert_eq!(panel(&v, Panel::WaveformEditor)["availability"], "unavailable");
        v.open(&wav).unwrap();
        press(&mut v, &ctx, egui::Key::F9, none);
        assert_eq!(panel(&v, Panel::SpectralDisplay)["availability"], "available");
        press(&mut v, &ctx, egui::Key::F11, none);
        press(&mut v, &ctx, egui::Key::F8, none);
        let _ = frame(&mut v, &ctx, vec![]);
        for (p, id) in [(Panel::WaveformEditor, "waveform.panel"), (Panel::SpectralDisplay, "spectral.panel"), (Panel::Mixer, "mixer.panel")] {
            assert_eq!(panel(&v, p)["visible"], true, "{p:?}: {}", v.state_json()["status"]);
            assert!(drew(&v, id), "{id} not drawn: {:?}", v.drawn());
        }
        // Hide and show again through the menu call.
        v.shell_command(ShellCommand::Panel(Panel::Mixer)).unwrap();
        let _ = frame(&mut v, &ctx, vec![]);
        assert!(!drew(&v, "mixer.panel"));
        v.shell_command(ShellCommand::Panel(Panel::Mixer)).unwrap();
        let _ = frame(&mut v, &ctx, vec![]);
        assert!(drew(&v, "mixer.panel"));
    }
    #[cfg(not(feature = "audio-batch"))]
    for (p, k) in [(Panel::Mixer, egui::Key::F8), (Panel::WaveformEditor, egui::Key::F9), (Panel::SpectralDisplay, egui::Key::F11)] {
        assert_eq!(panel(&v, p)["availability"], "needs_feature");
        assert!(panel(&v, p)["label"].as_str().unwrap().contains("needs feature audio-batch"));
        press(&mut v, &ctx, k, none);
        assert!(last_status(&v).contains("needs feature audio-batch"), "{}", last_status(&v));
        assert_eq!(panel(&v, p)["open"], false);
    }

    // Develop (F4) after opening a camera raw: shown, then hidden by its key.
    let dng = scratch.0.join("raw.dng");
    std::fs::write(&dng, synthetic_dng(64, 48)).unwrap();
    v.open(&dng).unwrap();
    let _ = frame(&mut v, &ctx, vec![]);
    assert!(drew(&v, "develop"), "{:?}", v.drawn());
    press(&mut v, &ctx, egui::Key::F4, none);
    let _ = frame(&mut v, &ctx, vec![]);
    assert!(!drew(&v, "develop"));
    assert_eq!(panel(&v, Panel::Develop)["open"], true);
}

#[test]
fn greyed_entries_name_their_feature() {
    let none = CompiledFeatures::default();
    assert_eq!(feature_gate(Panel::Mixer, &none), Availability::NeedsFeature("audio-batch"));
    assert_eq!(feature_gate(Panel::SourceMonitor, &none), Availability::NeedsFeature("media"));
    assert_eq!(entry_label(Panel::Mixer, &Availability::NeedsFeature("audio-batch")), "Mixer (needs feature audio-batch)");
    assert_eq!(entry_label(Panel::Mixer, &Availability::Unavailable("x".into())), "Mixer");
    let all = CompiledFeatures { media: true, audio_batch: true };
    for p in Panel::ALL {
        assert_eq!(feature_gate(p, &all), Availability::Available, "{p:?}");
    }
    // In this build: every entry whose feature is missing is greyed, says so and refuses typed.
    let mut v = Viewer::new(Prefer::Cpu);
    for p in Panel::ALL {
        let Some(f) = p.feature() else { continue };
        if CompiledFeatures::current().has(f) {
            continue;
        }
        let s = panel(&v, p);
        assert_eq!((s["availability"].as_str(), s["feature"].as_str()), (Some("needs_feature"), Some(f)));
        let e = v.toggle_panel(p).unwrap_err();
        assert!(e.contains(&format!("needs feature {f}")), "{e}");
    }
}

// ---- file dialog -----------------------------------------------------------------------------------

#[test]
fn file_dialog_lists_folders_first_and_filters_by_extension() {
    let scratch = Scratch::new("shell-dialog");
    let dir = &scratch.0;
    std::fs::create_dir(dir.join("sub")).unwrap();
    for name in ["b.PSD", "a.png", "notes.txt", "sub/c.jpg"] {
        std::fs::write(dir.join(name), b"x").unwrap();
    }
    let mut d = FileDialog::new(Purpose::Open, dir, None);
    let names = |d: &FileDialog| d.listing().entries.iter().map(|e| e.name.clone()).collect::<Vec<_>>();
    assert_eq!(names(&d), ["sub", "a.png", "b.PSD"], "folders first, filter by extension case-insensitively");
    let all = d.filters().len() - 1;
    d.set_filter(all).unwrap();
    assert_eq!(names(&d), ["sub", "a.png", "b.PSD", "notes.txt"]);
    assert_eq!(d.set_filter(99), Err("filter_out_of_range"));
    d.set_filter(0).unwrap();
    d.choose(0).unwrap();
    assert_eq!(names(&d), ["c.jpg"]);
    assert!(d.up());
    assert_eq!(d.choose(99), Err("entry_out_of_range"));
    // Typed paths: a folder lists it, a missing file is refused, a full path opens.
    d.typed = "sub".into();
    assert_eq!(d.submit(), Ok(None));
    assert_eq!(names(&d), ["c.jpg"]);
    d.up();
    d.typed = "missing.psd".into();
    assert_eq!(d.submit(), Err("file_not_found"));
    assert_eq!(d.error.as_deref(), Some("file_not_found"));
    d.typed = dir.join("a.png").to_string_lossy().into_owned();
    assert_eq!(d.submit(), Ok(Some(dir.join("a.png"))));
    // Bounded listing.
    let l = list_dir(dir, &file_dialog::Filter::ALL_FILES, 2).unwrap();
    assert!(l.truncated && l.entries.len() == 2);
    assert_eq!(list_dir(&dir.join("nope"), &file_dialog::Filter::ALL_FILES, 10), Err("dir_unreadable"));
    // Save: the default extension is added; an existing file needs replace; a missing folder is refused.
    let mut s = FileDialog::new(Purpose::SaveAs, dir, Some("out"));
    assert_eq!(s.submit(), Ok(Some(dir.join("out.handshake"))));
    std::fs::write(dir.join("out.handshake"), b"x").unwrap();
    assert_eq!(s.submit(), Err("file_exists"));
    s.replace = true;
    assert_eq!(s.submit(), Ok(Some(dir.join("out.handshake"))));
    s.typed = dir.join("no-such-dir").join("x").to_string_lossy().into_owned();
    assert_eq!(s.submit(), Err("save_dir_missing"));
    // Hostile typed paths.
    assert_eq!(parse_typed("   "), Err("path_empty"));
    assert_eq!(parse_typed("a\0b"), Err("path_control_character"));
    assert_eq!(parse_typed("a\nb"), Err("path_control_character"));
    assert_eq!(parse_typed(&"x".repeat(file_dialog::MAX_PATH_BYTES + 1)), Err("path_too_long"));
    assert_eq!(parse_typed("  a.psd "), Ok(PathBuf::from("a.psd")));
    // The dialog draws its stable ids inside the viewer.
    let mut v = Viewer::new(Prefer::Cpu);
    v.save_input = dir.join("x.handshake").to_string_lossy().into_owned();
    v.shell_command(ShellCommand::Open).unwrap();
    let ctx = egui::Context::default();
    let _ = frame(&mut v, &ctx, vec![]);
    for name in [file_dialog::widget::WINDOW, file_dialog::widget::UP, file_dialog::widget::FILTER, file_dialog::widget::PATH, file_dialog::widget::ACCEPT, file_dialog::widget::CANCEL] {
        assert!(drew(&v, name), "{name} not drawn: {:?}", v.drawn());
    }
    assert!(drew(&v, &file_dialog::widget::entry("a.png")), "{:?}", v.drawn());
    // Escape cancels.
    press(&mut v, &ctx, egui::Key::Escape, egui::Modifiers::NONE);
    assert!(v.state_json()["shell"]["dialog"].is_null());
}

// ---- recent files, save, export, close -------------------------------------------------------------

#[test]
fn open_recent_round_trips_and_hostile_lists_are_refused() {
    let scratch = Scratch::new("shell-recent");
    let (a, b) = (psd_at(&scratch.0, "a.psd"), psd_at(&scratch.0, "b.psd"));
    let mut v = Viewer::new(Prefer::Cpu);
    assert!(v.open(&scratch.0.join("missing.psd")).is_err());
    v.open(&a).unwrap();
    v.open(&b).unwrap();
    v.open(&a).unwrap();
    assert_eq!(v.recent_files(), [abs(&a), abs(&b)], "most recent first, no duplicates, failed opens not kept");
    // Round trip through bytes into another viewer, then reopen from the list.
    let bytes = v.recent_bytes();
    let mut w = Viewer::new(Prefer::Cpu);
    assert_eq!(w.load_recent(&bytes), Ok(2));
    assert_eq!(w.recent_files(), v.recent_files());
    let generation = w.recent_generation();
    w.shell_command(ShellCommand::OpenRecent(1)).unwrap();
    assert_eq!(w.state_json()["source"].as_str().map(PathBuf::from), Some(abs(&b)));
    assert_eq!(w.recent_files()[0], abs(&b));
    assert_ne!(w.recent_generation(), generation);
    assert!(w.shell_command(ShellCommand::OpenRecent(7)).is_err());
    // The Open Recent submenu draws one entry per file (drawn when opened through the shell model).
    w.shell_command(ShellCommand::ClearRecent).unwrap();
    assert!(w.recent_files().is_empty());
    // Hostile lists.
    let list = |paths: Value| serde_json::to_vec(&serde_json::json!({"schema": shell::RECENT_SCHEMA, "paths": paths})).unwrap();
    assert_eq!(decode_recent(&vec![b' '; shell::MAX_RECENT_BYTES + 1]), Err("recent_too_large"));
    assert_eq!(decode_recent(b"{not json"), Err("recent_json"));
    assert_eq!(decode_recent(br#"{"schema":"other@1","paths":[]}"#), Err("recent_schema"));
    assert_eq!(decode_recent(br#"{"schema":"hsk.studio.harness.recent@1","paths":[],"x":1}"#), Err("recent_keys"));
    assert_eq!(decode_recent(&list(serde_json::json!((0..11).map(|i| format!("f{i}.psd")).collect::<Vec<_>>()))), Err("recent_too_many"));
    assert_eq!(decode_recent(&list(serde_json::json!(["a\u{0}b.psd"]))), Err("recent_path"));
    assert_eq!(decode_recent(&list(serde_json::json!([3]))), Err("recent_path"));
    assert_eq!(decode_recent(&list(serde_json::json!(["x.psd", "x.psd", "y.psd"]))), Ok(vec![PathBuf::from("x.psd"), PathBuf::from("y.psd")]));
    assert_eq!(decode_recent(&encode_recent(&[])), Ok(vec![]));
    let mut x = Viewer::new(Prefer::Cpu);
    assert_eq!(x.load_recent(b"[]"), Err("recent_json"));
    assert!(x.recent_files().is_empty(), "a refused list changes nothing");
}

#[test]
fn save_as_export_and_close_run_through_the_dialog_and_shortcuts() {
    let scratch = Scratch::new("shell-save");
    let source = psd_at(&scratch.0, "two.psd");
    let mut v = Viewer::new(Prefer::Cpu);
    let ctx = egui::Context::default();
    // Nothing open: Save is refused with the reason in the status log.
    assert!(v.shell_command(ShellCommand::Save).is_err());
    assert!(last_status(&v).contains("nothing open"), "{}", last_status(&v));
    v.open(&source).unwrap();
    let _ = frame(&mut v, &ctx, vec![]);
    // Ctrl+Shift+S: Save As dialog with the suggested name; typing a bare name saves a .handshake.
    press(&mut v, &ctx, egui::Key::S, egui::Modifiers::COMMAND | egui::Modifiers::SHIFT);
    let s = v.state_json();
    assert_eq!((s["shell"]["dialog"]["purpose"].as_str(), s["shell"]["dialog"]["typed"].as_str()), (Some("save_as"), Some("two.handshake")));
    v.file_dialog().unwrap().typed = "copy".into();
    assert_eq!(v.dialog_accept(), Ok(true));
    let copy = scratch.0.join("copy.handshake");
    assert!(copy.is_file());
    let mut again = Viewer::new(Prefer::Cpu);
    again.open(&copy).unwrap();
    assert_eq!(again.layers().len(), v.layers().len());
    // Ctrl+S saves to the save path (now the copy) without a dialog.
    std::fs::remove_file(&copy).unwrap();
    press(&mut v, &ctx, egui::Key::S, egui::Modifiers::COMMAND);
    assert!(copy.is_file() && v.state_json()["shell"]["dialog"].is_null());
    // Export PNG: dialog, write, refusal to replace without the tick, replace with it.
    v.shell_command(ShellCommand::ExportPng).unwrap();
    assert_eq!(v.state_json()["shell"]["dialog"]["typed"], "copy.png");
    assert_eq!(v.dialog_accept(), Ok(true));
    let png = std::fs::read(scratch.0.join("copy.png")).unwrap();
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    v.shell_command(ShellCommand::ExportPng).unwrap();
    assert_eq!(v.dialog_accept(), Err("file_exists".into()));
    assert_eq!(v.state_json()["shell"]["dialog"]["error"], "file_exists");
    v.file_dialog().unwrap().replace = true;
    assert_eq!(v.dialog_accept(), Ok(true));
    // Export JPEG through the dialog.
    v.shell_command(ShellCommand::ExportJpeg).unwrap();
    assert_eq!(v.dialog_accept(), Ok(true));
    assert_eq!(&std::fs::read(scratch.0.join("copy.jpg")).unwrap()[..3], &[0xFF, 0xD8, 0xFF]);
    // Ctrl+O opens the Open dialog listing the folder; accepting a file opens it.
    press(&mut v, &ctx, egui::Key::O, egui::Modifiers::COMMAND);
    assert_eq!(v.state_json()["shell"]["dialog"]["purpose"], "open");
    v.file_dialog().unwrap().typed = "two.psd".into();
    assert_eq!(v.dialog_accept(), Ok(true));
    assert_eq!(v.state_json()["source"].as_str().map(PathBuf::from), Some(source.clone()));
    // View entries.
    v.shell_command(ShellCommand::ZoomIn).unwrap();
    v.shell_command(ShellCommand::ToggleStatus).unwrap();
    let _ = frame(&mut v, &ctx, vec![]);
    assert!(!drew(&v, "status"));
    v.shell_command(ShellCommand::ToggleStatus).unwrap();
    // Ctrl+W closes the document; the recent list stays.
    let recent = v.recent_files().to_vec();
    press(&mut v, &ctx, egui::Key::W, egui::Modifiers::COMMAND);
    let s = v.state_json();
    assert!(s["source"].is_null() && s["revision"].is_null() && s["layers"].as_array().unwrap().is_empty(), "{s}");
    assert_eq!(v.recent_files(), recent);
    assert!(v.shell_command(ShellCommand::Save).is_err());
    // Reopen after close works on the fresh run.
    v.open(&source).unwrap();
    assert_eq!(v.layers().len(), 2);
}

// ---- lane SMB-HARNESS-SHELL-2: naming law, HELP, one availability record -------------------------

fn menu_node(v: &Viewer, id: &str) -> Value {
    v.state_json()["shell"]["menu"]["nodes"].as_array().unwrap().iter().find(|n| n["id"] == id).cloned().unwrap_or_else(|| panic!("no menu node {id}"))
}

/// F-R37-3 / [STU-SHL-008] / [STU-SHL-251]: no vendor product name in any dialog filter label, dialog
/// item, help window item, menu title, submenu, leaf, panel label or command id, in every state the
/// tree changes with (nothing open, a document and a recent list open, a sequence document with
/// panels open). Operator file paths in Open Recent are data, not Studio names.
#[test]
fn no_vendor_product_name_in_dialog_filters_or_the_menu_tree() {
    let mut scanned = 0usize;
    for purpose in file_dialog::Purpose::ALL {
        for f in purpose.filters() {
            assert!(vendor_product_names(f.name).is_empty(), "{purpose:?} filter {:?} names {:?}", f.name, vendor_product_names(f.name));
            scanned += 1;
        }
        let w = purpose.window_spec();
        assert_eq!(w.audit(), vec![], "{purpose:?} window");
        scanned += w.items.len();
    }
    assert_eq!(file_dialog::Purpose::OpenPrototype.filters()[0].name, "Design file (.fig)");
    for page in [HelpPage::Manual, HelpPage::CommandIndex, HelpPage::Shortcuts, HelpPage::SystemReport, HelpPage::About] {
        assert_eq!(shell::help_window_spec(page).audit(), vec![], "{page:?}");
    }
    let scratch = Scratch::new("shell-naming");
    let mut v = Viewer::new(Prefer::Cpu);
    let mut check = |v: &Viewer, state: &str| {
        let bar = v.menu_bar();
        assert_eq!(bar.audit(), vec![], "{state}");
        for w in bar.walk() {
            let n = w.node;
            assert!(vendor_product_names(&n.id).is_empty(), "{state}: id {}", n.id);
            if !n.data_label {
                assert!(vendor_product_names(&n.label).is_empty(), "{state}: label {}", n.label);
            }
            scanned += 1;
        }
        // The projection agents read is the same tree.
        let nodes = v.state_json()["shell"]["menu"]["nodes"].as_array().unwrap().len();
        assert_eq!(nodes, bar.walk().len(), "{state}");
    };
    check(&v, "nothing open");
    let named = psd_at(&scratch.0, "figma-export.psd");
    v.open(&named).unwrap();
    check(&v, "document open, recent path holding a vendor word");
    v.open(&sequence_package(&scratch.0)).unwrap();
    v.shell_command(ShellCommand::Panel(Panel::LayoutEditor)).unwrap();
    v.shell_command(ShellCommand::Panel(Panel::ProgramMonitor)).unwrap();
    check(&v, "sequence document, panels open");
    assert!(scanned > 100, "scanned {scanned}");
}

/// [STU-SHL-037] HELP leaves in spec order, never hidden ([STU-SHL-018]); the implemented ones
/// open the in-window help view headlessly, the others refuse with NOT_IMPLEMENTED_YET.
#[test]
fn help_menu_carries_the_spec_leaves_and_opens_the_help_views() {
    let mut v = Viewer::new(Prefer::Cpu);
    let bar = v.menu_bar();
    let labels = |id: &str| bar.find(id).unwrap().children.iter().map(|c| c.label.clone()).collect::<Vec<_>>();
    let titles: Vec<&str> = bar.titles.iter().map(|t| t.label.as_str()).collect();
    assert_eq!(titles, ["File", "Edit", "View", "Workspace", "Help"], "HELP last ([STU-SHL-011] order)");
    assert_eq!(labels("help"), ["Studio Manual", "Full Command Index…", "Keyboard Shortcut Reference…", "Diagnostics", "About Studio"]);
    assert_eq!(labels(shell::HELP_MANUAL_ID), ["Open Studio Manual", "Search Manual…", "Help for This Command", "Help for This Panel", "Help for This Tool", "Domain Guides…"]);
    assert_eq!(
        labels(shell::HELP_DIAGNOSTICS_ID),
        ["Render Harness…", "Visual Debugger", "Flight Recorder", "Validation Report…", "System Report…", "GPU Report…", "Accessibility Tree Snapshot…"]
    );
    assert!(bar.find(HelpCommand::CommandIndex.id()).unwrap().menu_only && bar.find(shell::HELP_DIAGNOSTICS_ID).unwrap().menu_only);
    for h in HelpCommand::ALL {
        let node = bar.find(h.id()).unwrap();
        match h.page() {
            Some(_) => assert!(node.availability.is_available(), "{h:?}"),
            None => {
                assert_eq!((node.availability.state, node.availability.reason_code), (AvailabilityState::NotInThisDocument, Some(ReasonCode::NotImplementedYet)), "{h:?}");
                let e = v.shell_command(ShellCommand::Help(h)).unwrap_err();
                assert_eq!(e, node.availability.reason_line());
                assert!(v.help_state().is_none());
            }
        }
    }
    // Full Command Index: every node, with its id and availability.
    v.shell_command(ShellCommand::Help(HelpCommand::CommandIndex)).unwrap();
    let h = v.state_json()["shell"]["help"].clone();
    assert_eq!(h["page"], "command_index");
    let rows = h["rows"].as_array().unwrap();
    assert_eq!(rows.len(), bar.walk().len());
    let save = rows.iter().find(|r| r["id"] == "menu.studio.document.save").unwrap();
    assert!(save["title"] == "File > Save" && save["body"].as_str().unwrap().contains("INAPPLICABLE_HERE NO_SUCH_CONTAINER"), "{save}");
    // Keyboard Shortcut Reference: every shortcut of the tree.
    v.shell_command(ShellCommand::Help(HelpCommand::ShortcutReference)).unwrap();
    let rows = v.state_json()["shell"]["help"]["rows"].as_array().unwrap().clone();
    for (key, id) in [("Ctrl+O", "studio.document.open"), ("Ctrl+Shift+S", "studio.document.save_as"), ("F5", "studio.workspace.panel.layout_editor")] {
        assert!(rows.iter().any(|r| r["title"] == key && r["body"].as_str().unwrap().contains(id)), "{key}: {rows:?}");
    }
    // System report and about: the build's features and version.
    v.shell_command(ShellCommand::Help(HelpCommand::SystemReport)).unwrap();
    let rows = v.state_json()["shell"]["help"]["rows"].clone();
    assert!(rows.as_array().unwrap().iter().any(|r| r["id"] == "system.features" && r["body"].as_str().unwrap().contains("viewer")), "{rows}");
    v.shell_command(ShellCommand::Help(HelpCommand::About)).unwrap();
    assert!(v.state_json()["shell"]["help"]["rows"].as_array().unwrap().iter().any(|r| r["id"] == "about.version"));
    // The manual and its search: menu, window, panel-control and command-line topics.
    v.shell_command(ShellCommand::Help(HelpCommand::OpenManual)).unwrap();
    let all = v.state_json()["shell"]["help"]["rows"].as_array().unwrap().len();
    let ids: Vec<String> = v.state_json()["shell"]["help"]["rows"].as_array().unwrap().iter().map(|r| r["id"].as_str().unwrap().to_owned()).collect();
    for want in ["menu.studio.document.open", "dialog", "help", "cli.open"] {
        assert!(ids.iter().any(|i| i == want), "{want} missing from the manual");
    }
    let hits = v.help_search("  RECENT ").unwrap();
    assert!(hits > 0 && hits < all, "{hits} of {all}");
    for r in v.state_json()["shell"]["help"]["rows"].as_array().unwrap() {
        let text = format!("{} {}", r["title"], r["body"]).to_lowercase();
        assert!(text.contains("recent"), "{r}");
    }
    // Hostile queries are refused and change nothing.
    assert_eq!(v.help_search(&"x".repeat(shell::MAX_HELP_QUERY_BYTES + 1)), Err("help_query_too_long"));
    assert_eq!(v.help_search("a\u{0}b"), Err("help_query_control_character"));
    assert_eq!(v.help_search("a\nb"), Err("help_query_control_character"));
    assert_eq!(v.help_state().unwrap().query, "  RECENT ");
    assert_eq!(v.help_search("zzzz-no-such-topic"), Ok(0));
    // Drawn headlessly as an in-window egui window (no OS window, no focus request).
    let ctx = egui::Context::default();
    let _ = frame(&mut v, &ctx, vec![]);
    for name in [shell::widget::HELP_WINDOW, shell::widget::HELP_SEARCH, shell::widget::HELP_ROWS, shell::widget::HELP_CLOSE] {
        assert!(drew(&v, name), "{name} not drawn: {:?}", v.drawn());
    }
    v.help_close();
    let _ = frame(&mut v, &ctx, vec![]);
    assert!(!drew(&v, shell::widget::HELP_WINDOW) && v.state_json()["shell"]["help"].is_null());
}

/// [STU-SHL-019] / [STU-SHL-020] / MENU-INV-4: one availability record per node, the same in the
/// command API refusal, the agent JSON and (feature `desktop`) the AccessKit node description; an
/// INAPPLICABLE_HERE node names ONE enabled remedy, drawn as an enabled sibling leaf that runs it.
#[test]
fn availability_reaches_the_command_api_the_projection_and_accesskit_alike() {
    let mut v = Viewer::new(Prefer::Cpu);
    let save = menu_node(&v, "studio.document.save");
    assert_eq!(save["availability"]["availability_state"], "INAPPLICABLE_HERE");
    assert_eq!(save["availability"]["reason_code"], "NO_SUCH_CONTAINER");
    assert_eq!(save["availability"]["remedy_command_id"], "studio.document.open");
    let line = v.menu_bar().find("studio.document.save").unwrap().availability.reason_line();
    assert_eq!(v.shell_command(ShellCommand::Save), Err(line.clone()));
    assert!(last_status(&v).ends_with(&line), "{}", last_status(&v));
    // Develop: INAPPLICABLE_HERE with Open as remedy; program monitor without a sequence: NOT_IN_THIS_DOCUMENT.
    let dev = menu_node(&v, "studio.workspace.panel.develop");
    assert_eq!((dev["availability"]["availability_state"].as_str(), dev["availability"]["remedy_command_id"].as_str()), (Some("INAPPLICABLE_HERE"), Some("studio.document.open")));
    let scratch = Scratch::new("shell-availability");
    v.open(&psd_at(&scratch.0, "a.psd")).unwrap();
    let pm = menu_node(&v, "studio.workspace.panel.program_monitor");
    assert_eq!((pm["availability"]["availability_state"].as_str(), pm["availability"]["profile_clause"].as_str()), (Some("NOT_IN_THIS_DOCUMENT"), Some("container:timeline")));
    // Spectral display: its remedy resolves to an enabled leaf in every build.
    let sp = menu_node(&v, "studio.workspace.panel.spectral_display");
    match sp["availability"]["availability_state"].as_str() {
        Some("INAPPLICABLE_HERE") => {
            let remedy = sp["availability"]["remedy_command_id"].as_str().unwrap().to_owned();
            assert_eq!(menu_node(&v, &remedy)["availability"]["availability_state"], "AVAILABLE", "{remedy}");
        }
        Some("NOT_IN_THIS_DOCUMENT") => assert_eq!(sp["availability"]["reason_code"], "CAPABILITY_NOT_GRANTED"),
        other => panic!("spectral display {other:?}"),
    }
    assert_eq!(v.menu_bar().audit(), vec![]);
    // Redo after an undo-able step: INAPPLICABLE_HERE with Undo as remedy.
    let first = v.layers()[0].layer_id.clone();
    v.set_visible(&first, false).unwrap();
    let redo = menu_node(&v, "studio.history.redo");
    assert_eq!((redo["availability"]["availability_state"].as_str(), redo["availability"]["remedy_command_id"].as_str()), (Some("INAPPLICABLE_HERE"), Some("studio.history.undo")));
    accesskit_reasons();
}

#[cfg(not(feature = "desktop"))]
fn accesskit_reasons() {
    println!("NOT_PROVEN: AccessKit node descriptions need feature desktop (egui accesskit); proven in the desktop unit");
}

/// Feature `desktop`: File opened through AccessKit; the greyed Save node is disabled and its
/// description is the reason line; the remedy sibling node runs Open.
#[cfg(feature = "desktop")]
fn accesskit_reasons() {
    let mut v = Viewer::new(Prefer::Cpu);
    let ctx = egui::Context::default();
    ctx.enable_accesskit();
    let out = frame(&mut v, &ctx, vec![]);
    let (file, _) = ak_find(&out, |l| l == "File").expect("File menu node");
    let _ = frame(&mut v, &ctx, vec![ak_click(file)]);
    let out = frame(&mut v, &ctx, vec![]);
    let tree = out.platform_output.accesskit_update.as_ref().expect("accesskit update");
    let line = v.menu_bar().find("studio.document.save").unwrap().availability.reason_line();
    let save = tree.nodes.iter().find(|(_, n)| n.label().is_some_and(|l| l.starts_with("Save") && !l.starts_with("Save As"))).map(|(_, n)| n.clone()).expect("Save node");
    assert!(save.is_disabled());
    assert_eq!(save.description(), Some(line.as_str()));
    let open = tree.nodes.iter().find(|(_, n)| n.label().is_some_and(|l| l.starts_with("Open…"))).map(|(_, n)| n.clone()).expect("Open node");
    assert_eq!(open.description(), Some("AVAILABLE"));
    let (remedy, disabled) = ak_find(&out, |l| l.starts_with("-> Open")).expect("remedy sibling node");
    assert!(!disabled);
    let _ = frame(&mut v, &ctx, vec![ak_click(remedy)]);
    assert_eq!(v.state_json()["shell"]["dialog"]["purpose"], "open", "{}", v.state_json()["status"]);
}
