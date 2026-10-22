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
use hsk_studio_harness::view::shell::{self, Availability, CompiledFeatures, Panel, ShellCommand, decode_recent, encode_recent, entry_label, feature_gate, menu_entries};
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
    for name in [shell::widget::MENU_BAR, shell::widget::FILE, shell::widget::EDIT, shell::widget::VIEW, shell::widget::WORKSPACE] {
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
    assert_eq!(s["shell"]["dialog"]["filter"], "Figma (.fig)");
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
