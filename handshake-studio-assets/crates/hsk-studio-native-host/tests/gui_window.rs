//! Feature `document`, on the real device (GPU test group; validation: absent): the GUI frame offscreen (no HWND) and
//! in a quiet window give the same frame hash after the same scripted pointer session, with no focus change; the
//! exercise run resizes the pane with the window, skips the minimised frames and restores the same frame. With
//! HSK_NATIVE_HOST_GUI_OUT set, the PNGs, receipts, tree files and the fixture .handshake are copied there.
#![cfg(feature = "document")]
mod common;

use common::{Scratch, fixture};
use hsk_studio_native_host::gui::{GuiFrame, row_id};
use hsk_studio_native_host::offscreen::{self, OffscreenArgs};
use hsk_studio_native_host::scene::{GuiArgs, Scene};
use hsk_studio_native_host::script::{Step, click};
use hsk_studio_native_host::window::{self, FocusReport, WindowArgs};
use std::path::Path;
use std::time::Duration;

const SIZE: (u32, u32) = (800, 480);

fn assert_quiet(f: &FocusReport) {
    assert_eq!(f.focus_changes, 0, "{f:?}");
    assert_eq!(f.foreground_takes, 0, "{f:?}");
    assert!(f.no_activate_style && f.tool_window_style, "{f:?}");
    assert!(!f.app_window_style, "{f:?}");
}

/// A pointer session computed on a CPU frame of the same size and scale: select Green, hide Red with its eye.
fn session(doc: &Path) -> Vec<Step> {
    let mut f = GuiFrame::open(doc, SIZE.0, SIZE.1, Some(1000)).unwrap();
    f.frame().unwrap();
    let id = |name: &str| f.rows().iter().find(|r| r.name == name).map(|r| row_id(&r.layer_id)).unwrap();
    let (green, red) = (id("Green"), id("Red"));
    let mut steps = Vec::new();
    for target in [green, format!("{red}.visibility")] {
        let (x, y) = f.centre(&target).unwrap();
        steps.extend(click(x, y));
    }
    steps
}

fn keep(dir: &Path, names: &[&str]) {
    let Some(out) = std::env::var_os("HSK_NATIVE_HOST_GUI_OUT") else { return };
    let out = std::path::PathBuf::from(out);
    std::fs::create_dir_all(&out).unwrap();
    for n in names {
        if dir.join(n).is_file() {
            std::fs::copy(dir.join(n), out.join(n)).unwrap();
            println!("kept {}", out.join(n).display());
        }
    }
}

#[test]
fn gui_offscreen_png_equals_quiet_window_capture_after_a_pointer_session() {
    let s = Scratch::new("window");
    let doc = fixture(&s.0);
    let steps = session(&doc);
    let gui = |tree: &str| GuiArgs { steps: steps.clone(), tree: Some(s.0.join(tree)), scale_milli: Some(1000) };
    let off = offscreen::run(&OffscreenArgs {
        width: SIZE.0,
        height: SIZE.1,
        scene: Scene::Gui(doc.clone()),
        out: Some(s.0.join("gui-offscreen.png")),
        receipt: Some(s.0.join("gui-offscreen.json")),
        allow_overlays: false,
        gui: gui("gui-offscreen-tree.json"),
    })
    .expect("gui offscreen");
    assert!(off.exact, "readback equals the painted frame");
    assert_eq!((off.width, off.height), SIZE);
    assert_eq!(off.receipt.get("hwnd_created").map(|v| v.render()), Some("false".into()));
    let g = off.receipt.get("gui").expect("gui member");
    assert_eq!(g.get("last_command").map(|v| v.render()), Some(r#"{"command":"layer.set_visible","code":"ok"}"#.into()));

    let win = window::run(&WindowArgs {
        width: SIZE.0,
        height: SIZE.1,
        scene: Scene::Gui(doc.clone()),
        quiet: true,
        frames: 3,
        exercise: false,
        capture: Some(s.0.join("gui-swapchain.png")),
        receipt: Some(s.0.join("gui-swapchain.json")),
        allow_overlays: false,
        deadline: Duration::from_secs(120),
        gui: gui("gui-swapchain-tree.json"),
    })
    .expect("gui quiet window");
    keep(&s.0, &["gui-offscreen.png", "gui-offscreen.json", "gui-offscreen-tree.json", "gui-swapchain.png", "gui-swapchain.json", "gui-swapchain-tree.json", "layered.handshake"]);
    assert_quiet(&win.focus);
    assert_eq!(win.swap_extent, SIZE);
    if win.frame_sha256.as_deref() != Some(off.frame_sha256.as_str()) {
        eprintln!("window events: {:?}", win.events);
        eprintln!("window gui: {}", win.receipt.get("gui").map(|v| v.render()).unwrap_or_default());
        eprintln!("offscreen gui: {}", g.render());
    }
    assert_eq!(win.frame_sha256.as_deref(), Some(off.frame_sha256.as_str()), "swapchain frame equals the offscreen frame");
    let a = std::fs::read_to_string(s.0.join("gui-offscreen-tree.json")).unwrap();
    let b = std::fs::read_to_string(s.0.join("gui-swapchain-tree.json")).unwrap();
    assert_eq!(a, b, "the same tree after the same session");
    let receipt = std::fs::read_to_string(s.0.join("gui-swapchain.json")).unwrap();
    assert!(receipt.contains(r#""scene":"gui""#) && receipt.contains(r#""chrome":"cpu painter, Vulkan UI pass pending""#), "{receipt}");
}

#[test]
fn gui_quiet_exercise_resizes_the_pane_and_restores_the_frame() {
    let s = Scratch::new("exercise");
    let doc = fixture(&s.0);
    let gui = GuiArgs { steps: Vec::new(), tree: None, scale_milli: Some(1000) };
    let off = offscreen::run(&OffscreenArgs {
        width: SIZE.0,
        height: SIZE.1,
        scene: Scene::Gui(doc.clone()),
        out: None,
        receipt: None,
        allow_overlays: false,
        gui: gui.clone(),
    })
    .expect("gui offscreen");
    let win = window::run(&WindowArgs {
        width: SIZE.0,
        height: SIZE.1,
        scene: Scene::Gui(doc),
        quiet: true,
        frames: 4,
        exercise: true,
        capture: Some(s.0.join("gui-exercise.png")),
        receipt: None,
        allow_overlays: false,
        deadline: Duration::from_secs(120),
        gui,
    })
    .expect("gui exercise");
    assert_quiet(&win.focus);
    assert_eq!(win.exercised, ["resize_grow", "minimize_no_activate", "restore_no_activate", "resize_back"]);
    assert!(win.counters.skipped_minimized >= 1, "{:?}", win.counters);
    let g = win.receipt.get("gui").expect("gui member");
    let uploads: u64 = g.get("uploads").map(|v| v.render()).unwrap().parse().unwrap();
    assert!(uploads >= 3, "the pane followed the window: {uploads}");
    assert_eq!(win.frame_sha256.as_deref(), Some(off.frame_sha256.as_str()), "back at the first size: the same frame");
}
