//! Feature `document`, on the real device (GPU test group; validation on in debug builds when the loader lists the
//! Khronos layer, zero errors): the GUI frame offscreen (no HWND) and
//! in a quiet window give the same frame hash after the same scripted pointer session, with no focus change; the
//! exercise run resizes the pane with the window, skips the minimised frames and restores the same frame; the raster
//! frame of SMB-GUI-FRAME-1 at 1920 x 1080 and 1280 x 720 is equal offscreen and in the window. With
//! HSK_NATIVE_HOST_GUI_OUT set, the PNGs, receipts, tree files and the fixture .handshake are copied there.
#![cfg(feature = "document")]
mod common;
mod validation;

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
    let gui = |tree: &str| GuiArgs { steps: steps.clone(), tree: Some(s.0.join(tree)), scale_milli: Some(1000), ..GuiArgs::default() };
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
    assert!(receipt.contains(r#""scene":"gui""#) && receipt.contains(r#""chrome":"cpu painter""#), "{receipt}");
    validation::assert_clean(&off.receipt);
    validation::assert_clean(&win.receipt);
}

#[test]
fn gui_quiet_exercise_resizes_the_pane_and_restores_the_frame() {
    let s = Scratch::new("exercise");
    let doc = fixture(&s.0);
    let gui = GuiArgs { steps: Vec::new(), tree: None, scale_milli: Some(1000), ..GuiArgs::default() };
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
    validation::assert_clean(&off.receipt);
    validation::assert_clean(&win.receipt);
}

/// SMB-GUI-FRAME-1 captures (HBR-VIS-003 matrix): the raster frame at `w x h` scale 1 offscreen and in a quiet window
/// with equal frame_sha256 and no focus change; receipts show missing_resources 0 and glyph_refusals 0. One window per
/// test process (winit: one event loop per process). With HSK_NATIVE_HOST_GUI_OUT set, the PNGs, receipts and tree
/// files are kept there.
fn frame_capture(w: u32, h: u32) {
    let s = Scratch::new(&format!("frame-{w}x{h}"));
    let doc = fixture(&s.0);
    let tag = format!("frame-{w}x{h}");
    let gui = |tree: &str| GuiArgs { steps: Vec::new(), tree: Some(s.0.join(tree)), scale_milli: Some(1000), ..GuiArgs::default() };
    let off = offscreen::run(&OffscreenArgs {
        width: w,
        height: h,
        scene: Scene::Gui(doc.clone()),
        out: Some(s.0.join(format!("{tag}-offscreen.png"))),
        receipt: Some(s.0.join(format!("{tag}-offscreen.json"))),
        allow_overlays: false,
        gui: gui(&format!("{tag}-offscreen-tree.json")),
    })
    .expect("frame offscreen");
    assert!(off.exact, "{tag}: readback equals the painted frame");
    let paint = off.receipt.get("gui").and_then(|g| g.get("frame")).and_then(|f| f.get("paint")).map(|p| p.render()).unwrap_or_default();
    assert!(paint.contains(r#""glyph_refusals":0"#) && paint.contains(r#""missing_resources":0"#), "{tag}: {paint}");
    let win = window::run(&WindowArgs {
        width: w,
        height: h,
        scene: Scene::Gui(doc.clone()),
        quiet: true,
        frames: 3,
        exercise: false,
        capture: Some(s.0.join(format!("{tag}-swapchain.png"))),
        receipt: Some(s.0.join(format!("{tag}-swapchain.json"))),
        allow_overlays: false,
        deadline: Duration::from_secs(120),
        gui: gui(&format!("{tag}-swapchain-tree.json")),
    })
    .expect("frame quiet window");
    let names: Vec<String> = ["offscreen.png", "offscreen.json", "offscreen-tree.json", "swapchain.png", "swapchain.json", "swapchain-tree.json"].iter().map(|k| format!("{tag}-{k}")).collect();
    let mut names: Vec<&str> = names.iter().map(String::as_str).collect();
    if w == 1920 {
        // The band rects of the 1920 x 1080 frame (AC-LOOK-02 machine half, for the designer's comparison).
        let mut f = GuiFrame::open(&doc, 1920, 1080, Some(1000)).unwrap();
        f.frame().unwrap();
        let bands: Vec<String> = f.layout().regions().iter().map(|(name, r)| format!("{{\"region\":\"{name}\",\"rect\":[{},{},{},{}]}}", r.x0, r.y0, r.x1, r.y1)).collect();
        std::fs::write(s.0.join("frame-1920x1080-bands.json"), format!("{{\"schema\":\"hsk.studio.native_host.bands@1\",\"window\":[1920,1080],\"scale\":1,\"bands\":[{}]}}\n", bands.join(","))).unwrap();
        names.push("frame-1920x1080-bands.json");
    }
    keep(&s.0, &names);
    assert_quiet(&win.focus);
    assert_eq!(win.swap_extent, (w, h), "{tag}");
    assert_eq!(win.frame_sha256.as_deref(), Some(off.frame_sha256.as_str()), "{tag}: swapchain frame equals the offscreen frame");
}

#[test]
fn frame_capture_1920x1080_offscreen_equals_quiet_window() {
    frame_capture(1920, 1080);
}

#[test]
fn frame_capture_1280x720_offscreen_equals_quiet_window() {
    frame_capture(1280, 720);
}

/// FIT-L1: the expanded Browse Dock at 2560 x 1440 scale 1.
#[test]
fn frame_capture_2560x1440_offscreen_equals_quiet_window() {
    frame_capture(2560, 1440);
}

#[test]
fn frame_capture_1024x640_offscreen_equals_quiet_window() {
    frame_capture(1024, 640);
}

/// The edge state offscreen at 1024 x 640 after a script: a tool picked, a layer hidden, the Properties group on
/// Masks, a menu title clicked (the status shows not_implemented_yet).
#[test]
fn frame_capture_1024x640_edge_state_after_a_script() {
    let s = Scratch::new("frame-edge");
    let doc = fixture(&s.0);
    let mut g = GuiFrame::open(&doc, 1024, 640, Some(1000)).unwrap();
    g.frame().unwrap();
    let red = g.rows().iter().find(|r| r.name == "Red").map(|r| row_id(&r.layer_id)).unwrap();
    let (mx, my) = g.centre("menu.file").unwrap();
    let mut steps = vec![Step::Invoke("studio.toolgroup.paint".into()), Step::Invoke(format!("{red}.visibility")), Step::Invoke("studio.tab.masks".into())];
    steps.extend(click(mx, my));
    let off = offscreen::run(&OffscreenArgs {
        width: 1024,
        height: 640,
        scene: Scene::Gui(doc.clone()),
        out: Some(s.0.join("frame-1024x640-edge-offscreen.png")),
        receipt: Some(s.0.join("frame-1024x640-edge-offscreen.json")),
        allow_overlays: false,
        gui: GuiArgs { steps, tree: Some(s.0.join("frame-1024x640-edge-offscreen-tree.json")), scale_milli: Some(1000), ..GuiArgs::default() },
    })
    .expect("edge offscreen");
    keep(&s.0, &["frame-1024x640-edge-offscreen.png", "frame-1024x640-edge-offscreen.json", "frame-1024x640-edge-offscreen-tree.json"]);
    let gr = off.receipt.get("gui").unwrap().render();
    assert!(gr.contains(r#""last_command":{"command":"frame.static","code":"not_implemented_yet"}"#), "{gr}");
    assert!(gr.contains(r#""tool_active":"paint_brush""#) && gr.contains(r#""active_tabs":["masks","colour","layers"]"#), "{gr}");
    assert!(gr.contains(r#""glyph_refusals":0"#) && gr.contains(r#""missing_resources":0"#), "{gr}");
}
