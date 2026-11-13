//! Feature `document`: the `--chrome vulkan` GUI frame (SMB-VK-UI-PASS-1 items 7 and 8 c to f). The switch is gui
//! only and typed; a target that loses the device ends the drive with exit 3 and no later command; on the real
//! device (GPU test group, validation on, zero errors) the spine's frame drawn by the UI pass matches the CPU
//! painter's frame from the same compose call at four scales, exact outside the AA rects; offscreen and quiet window
//! give the same hash, two runs the same bytes; the quiet exercise restores the frame without new glyph uploads; idle
//! frames add no pass, instance or upload. With HSK_NATIVE_HOST_GUI_OUT set, PNGs and receipts are copied there.
#![cfg(feature = "document")]
mod common;
mod validation;

use common::{Scratch, fixture};
use hsk_studio_native_host::HostError;
use hsk_studio_native_host::capture::Json;
use hsk_studio_native_host::cli::{self, Command};
use hsk_studio_native_host::gui::{self, Chrome, ChromeTarget, CpuChrome, GuiFrame, VulkanChrome, row_id};
use hsk_studio_native_host::offscreen::{self, OffscreenArgs};
use hsk_studio_native_host::scene::{ChromeMode, GuiArgs, Scene};
use hsk_studio_native_host::script::{Step, click};
use hsk_studio_native_host::window::{self, FocusReport, WindowArgs};
use hsk_studio_vulkan::{GpuClientPort, Owner, OwnerConfig, Refusal, UiPass};
use std::path::Path;
use std::time::Duration;
use hsk_studio_accord::CancellationToken;
use hsk_studio_native_host::cli::Caller;
use hsk_studio_native_host::diag::HostRun;
use hsk_studio_observe::{DeliveryClass, DeliveryError, SinkPort};

/// One host entry as the asset-local caller: the run sends exactly one terminal frame (F-R60-22).
fn observed<T>(run: impl FnOnce(&mut HostRun<'_>) -> T) -> T {
    let (cancel, mut frames) = (CancellationToken::default(), Frames::default());
    let caller = Caller::asset_local();
    let out = run(&mut HostRun::new(caller.actor, caller.resource, 1, &cancel, &mut frames).expect("host run"));
    assert_eq!(frames.0, [DeliveryClass::Terminal], "one terminal frame per host run");
    out
}

/// The classes of the frames a host run sent.
#[derive(Default)]
struct Frames(Vec<DeliveryClass>);

impl SinkPort for Frames {
    fn try_send(&mut self, class: DeliveryClass, _: &[u8]) -> Result<(), DeliveryError> {
        self.0.push(class);
        Ok(())
    }
}

const SIZE: (u32, u32) = (800, 480);

fn assert_quiet(f: &FocusReport) {
    assert_eq!(f.focus_changes, 0, "{f:?}");
    assert_eq!(f.foreground_takes, 0, "{f:?}");
    assert!(f.no_activate_style && f.tool_window_style && !f.app_window_style, "{f:?}");
}

fn num(j: &Json, path: &[&str]) -> u64 {
    let mut v = j;
    for k in path {
        v = v.get(k).unwrap_or_else(|| panic!("{path:?} missing in {}", j.render()));
    }
    v.render().parse().unwrap_or_else(|_| panic!("{path:?} not a number: {}", v.render()))
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

/// Select Green, hide Red with its eye: computed on a CPU frame of the same size and scale.
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

fn vulkan_gui(steps: Vec<Step>, tree: Option<std::path::PathBuf>) -> GuiArgs {
    GuiArgs { steps, tree, scale_milli: Some(1000), chrome: ChromeMode::Vulkan, ui_trace: None }
}

#[test]
fn chrome_flag_is_gui_only_and_typed() {
    let Ok(Command::Offscreen(o)) = cli::parse(&["offscreen", "--scene", "gui", "--document", "d.handshake", "--chrome", "vulkan", "--ui-trace", "t.json"]) else {
        panic!("parse")
    };
    assert_eq!((o.gui.chrome, o.gui.ui_trace.is_some()), (ChromeMode::Vulkan, true));
    let Ok(Command::Window(w)) = cli::parse(&["window", "--scene", "gui", "--document", "d", "--chrome", "cpu"]) else { panic!("window") };
    assert_eq!(w.gui.chrome, ChromeMode::Cpu);
    let Ok(Command::Offscreen(d)) = cli::parse(&["offscreen", "--scene", "gui", "--document", "d"]) else { panic!("default") };
    assert_eq!(d.gui.chrome, ChromeMode::Cpu, "the CPU painter is the default");
    let bad = cli::parse(&["offscreen", "--scene", "gui", "--document", "d", "--chrome", "metal"]).err();
    assert_eq!(bad, Some(HostError::Usage("chrome")));
    assert_eq!(bad.map(HostError::exit_code), Some(2));
    assert_eq!(cli::parse(&["offscreen", "--chrome", "vulkan"]).err(), Some(HostError::Usage("gui_only")));
    assert_eq!((ChromeMode::Cpu.code(), ChromeMode::Vulkan.code()), ("cpu painter", "vulkan ui pass"));
}

/// Hostile (h): a Vulkan target that loses the device ends the drive with exit 3; no Viewer command runs after it and
/// nothing retries. Vulkan mode keeps no canvas.
#[test]
fn a_lost_vulkan_target_ends_the_drive_with_exit_3() {
    struct Lost {
        calls: u32,
    }
    impl ChromeTarget for Lost {
        fn draw(&mut self, c: Chrome<'_>) -> Result<(), HostError> {
            assert!(!c.lists.is_empty());
            self.calls += 1;
            if self.calls >= 2 { Err(HostError::Vulkan(Refusal::DeviceLost)) } else { Ok(()) }
        }
    }
    let s = Scratch::new("vk-lost");
    let mut f = GuiFrame::open(&fixture(&s.0), SIZE.0, SIZE.1, Some(1000)).unwrap();
    f.set_chrome(ChromeMode::Vulkan);
    assert_eq!(f.chrome(), ChromeMode::Vulkan);
    let red = f.rows().iter().find(|r| r.name == "Red").map(|r| row_id(&r.layer_id)).unwrap();
    let green = f.rows().iter().find(|r| r.name == "Green").map(|r| row_id(&r.layer_id)).unwrap();
    let steps = [Step::Invoke(red), Step::Invoke(format!("{green}.visibility")), Step::Invoke(green)];
    let mut lost = Lost { calls: 0 };
    let err = gui::drive_chrome(&mut f, &mut lost, &steps).unwrap_err();
    assert_eq!((err, err.exit_code()), (HostError::Vulkan(Refusal::DeviceLost), 3));
    assert_eq!(lost.calls, 2, "no retry");
    assert_eq!(f.stats().commands, 1, "no Viewer command after the loss");
    assert!(f.rgba().is_none(), "Vulkan mode keeps no canvas");
    let label = f.node("studio.status.device").unwrap().label.clone().unwrap();
    assert!(label.ends_with("; chrome vulkan"), "{label}");
}

/// Item 8(c): the spine's frame (layered fixture, Inter text) at 800 x 480 and four scales, before and after a
/// command: the pane the UI pass drew against the CPU painter's canvas from one build of the lists (compose); every
/// pixel outside the AA rects of the trace is exact.
#[test]
fn spine_frame_on_the_ui_pass_matches_the_cpu_painter_at_four_scales() {
    let s = Scratch::new("vk-spine");
    let doc = fixture(&s.0);
    let mut owner = Owner::open(OwnerConfig::default()).expect("Vulkan 1.3 device");
    let mut pass = UiPass::new(&owner).expect("ui pass");
    pass.set_trace(true);
    let mut report = Vec::new();
    for milli in [1000u32, 1250, 1500, 2000] {
        let mut f = GuiFrame::open(&doc, SIZE.0, SIZE.1, Some(milli)).unwrap();
        f.set_chrome(ChromeMode::Vulkan);
        assert_eq!(f.text_kind(), "inter");
        for round in 0..2 {
            if round == 1 {
                let red = f.rows().iter().find(|r| r.name == "Red").map(|r| row_id(&r.layer_id)).unwrap();
                f.invoke(&format!("{red}.visibility")).unwrap();
            }
            assert!(f.frame().unwrap());
            let pane = owner.create_pane(SIZE.0, SIZE.1).unwrap();
            assert!(f.draw(&mut VulkanChrome { pass: &mut pass, owner: &mut owner, pane }).unwrap());
            let got = owner.read_pane(pane).unwrap();
            owner.release_pane(pane).unwrap();
            let mut cpu = CpuChrome::default();
            f.compose(&mut cpu).unwrap();
            let want = cpu.canvas.unwrap().rgba().to_vec();
            let (max, n, outside) = gui::parity(&want, &got, SIZE.0, pass.trace());
            report.push(format!("{milli}/{round}: max {max} differing {n} outside_aa {outside}"));
            // Review F6: measured 0 at every scale; the bound is the measurement.
            assert_eq!(max, 0, "{milli} round {round}: max {max} ({n} px)");
            assert_eq!(outside, 0, "{milli} round {round}: pixels outside the AA rects differ");
        }
    }
    println!("spine parity {}", report.join("; "));
    drop(pass);
    drop(owner);
    let v = hsk_studio_vulkan::process_validation();
    println!("validation {} errors {} warnings {}", v.state, v.errors, v.warnings);
    if v.state == "on" {
        assert_eq!((v.errors, v.warnings), (0, 0), "{v:?}");
        assert!(v.loader.iter().all(|n| hsk_studio_vulkan::is_expected_loader_note(&n.name, &n.text)), "{v:?}");
    }
}

/// Item 8(d): offscreen and quiet-window captures of the Vulkan chrome have equal frame_sha256 after the same
/// session, two offscreen runs give equal bytes, and the receipt carries ui_pass (SPIR-V hashes, features, counters,
/// parity) with chrome "vulkan ui pass".
#[test]
fn vulkan_offscreen_equals_the_quiet_window_and_two_runs_agree() {
    let s = Scratch::new("vk-window");
    let doc = fixture(&s.0);
    let steps = session(&doc);
    let off_args = |out: &str, receipt: &str, tree: &str| OffscreenArgs {
        width: SIZE.0,
        height: SIZE.1,
        scene: Scene::Gui(doc.clone()),
        out: Some(s.0.join(out)),
        receipt: Some(s.0.join(receipt)),
        allow_overlays: false,
        gui: vulkan_gui(steps.clone(), Some(s.0.join(tree))),
    };
    let mut first = off_args("gui-vulkan-offscreen.png", "gui-vulkan-offscreen.json", "gui-vulkan-offscreen-tree.json");
    first.gui.ui_trace = Some(s.0.join("gui-vulkan-trace.json"));
    let off = observed(|host_run| offscreen::run(&first, host_run)).expect("vulkan offscreen");
    let trace = std::fs::read_to_string(s.0.join("gui-vulkan-trace.json")).unwrap();
    assert!(trace.starts_with(r#"{"schema":"hsk.studio.native_host.ui_trace@1","records":[{"kind":"#), "{}", &trace[..trace.len().min(200)]);
    assert!(trace.contains(r#""class":"glyph""#) && trace.contains(r#""class":"flat""#));
    for png in ["gui-vulkan-offscreen.cpu.png", "gui-vulkan-offscreen.diff.png"] {
        assert!(s.0.join(png).is_file(), "{png}: the CPU reference and the difference image beside --out");
    }
    let again = observed(|host_run| offscreen::run(&off_args("gui-vulkan-offscreen-2.png", "gui-vulkan-offscreen-2.json", "gui-vulkan-offscreen-2-tree.json"), host_run)).expect("second run");
    assert_eq!(off.rgba8, again.rgba8, "two runs, equal bytes");
    assert_eq!(off.receipt.get("chrome").map(Json::render), Some(r#""vulkan ui pass""#.into()));
    let ui = off.receipt.get("ui_pass").expect("ui_pass member");
    assert_eq!(ui.get("manifest_match").map(Json::render), Some("true".into()), "{}", ui.render());
    assert_eq!(ui.get("naga").map(Json::render), Some(r#""27.0.3""#.into()));
    assert!(ui.get("features").map(Json::render).is_some_and(|f| f.contains("dynamicRendering")), "{}", ui.render());
    assert!(num(ui, &["passes"]) >= 1 && num(ui, &["instances"]) > 0 && num(ui, &["glyph_uploads"]) > 0, "{}", ui.render());
    assert!(num(ui, &["parity", "max"]) == 0 && num(ui, &["parity", "outside_aa_max"]) == 0, "{}", ui.render());
    let win = observed(|host_run| window::run(&WindowArgs {
        width: SIZE.0,
        height: SIZE.1,
        scene: Scene::Gui(doc.clone()),
        quiet: true,
        frames: 3,
        exercise: false,
        capture: Some(s.0.join("gui-vulkan-swapchain.png")),
        receipt: Some(s.0.join("gui-vulkan-swapchain.json")),
        allow_overlays: false,
        deadline: Duration::from_secs(120),
        gui: vulkan_gui(steps.clone(), Some(s.0.join("gui-vulkan-swapchain-tree.json"))),
    }, host_run))
    .expect("vulkan quiet window");
    keep(
        &s.0,
        &[
            "gui-vulkan-offscreen.png",
            "gui-vulkan-offscreen.cpu.png",
            "gui-vulkan-offscreen.diff.png",
            "gui-vulkan-offscreen.json",
            "gui-vulkan-trace.json",
            "gui-vulkan-swapchain.png",
            "gui-vulkan-swapchain.json",
            "layered.handshake",
        ],
    );
    assert_quiet(&win.focus);
    assert_eq!(win.frame_sha256.as_deref(), Some(off.frame_sha256.as_str()), "swapchain frame equals the offscreen frame");
    assert_eq!(win.receipt.get("chrome").map(Json::render), Some(r#""vulkan ui pass""#.into()));
    let a = std::fs::read_to_string(s.0.join("gui-vulkan-offscreen-tree.json")).unwrap();
    let b = std::fs::read_to_string(s.0.join("gui-vulkan-swapchain-tree.json")).unwrap();
    assert_eq!(a, b, "the semantic tree does not depend on the chrome target");
    validation::assert_clean(&off.receipt);
    validation::assert_clean(&win.receipt);
}

/// Item 8(e): the quiet exercise in Vulkan mode (resize, minimise, restore, back) ends on the offscreen frame, and no
/// glyph is uploaded twice (the restore and the way back add no upload for unchanged text).
#[test]
fn vulkan_quiet_exercise_restores_the_frame_without_new_glyph_uploads() {
    let s = Scratch::new("vk-exercise");
    let doc = fixture(&s.0);
    let off = observed(|host_run| offscreen::run(&OffscreenArgs {
        width: SIZE.0,
        height: SIZE.1,
        scene: Scene::Gui(doc.clone()),
        out: None,
        receipt: None,
        allow_overlays: false,
        gui: vulkan_gui(Vec::new(), None),
    }, host_run))
    .expect("vulkan offscreen");
    let win = observed(|host_run| window::run(&WindowArgs {
        width: SIZE.0,
        height: SIZE.1,
        scene: Scene::Gui(doc),
        quiet: true,
        frames: 4,
        exercise: true,
        capture: Some(s.0.join("gui-vulkan-exercise.png")),
        receipt: None,
        allow_overlays: false,
        deadline: Duration::from_secs(120),
        gui: vulkan_gui(Vec::new(), None),
    }, host_run))
    .expect("vulkan exercise");
    assert_quiet(&win.focus);
    assert_eq!(win.exercised, ["resize_grow", "minimize_no_activate", "restore_no_activate", "resize_back"]);
    let ui = win.receipt.get("ui_pass").expect("ui_pass member");
    assert!(num(ui, &["draws"]) >= 3, "the pane followed the window: {}", ui.render());
    // The grown window may show more text (the fit ladder, SMB-GUI-FRAME-1), so it may upload new glyphs; the
    // minimise, the restore and the way back upload none again: every resident glyph was uploaded once, on one page.
    let first = num(off.receipt.get("ui_pass").unwrap(), &["glyph_uploads"]);
    assert!(num(ui, &["glyph_uploads"]) >= first, "{}", ui.render());
    assert_eq!(num(ui, &["glyph_uploads"]), num(ui, &["glyph_entries"]), "no glyph uploaded twice: {}", ui.render());
    assert_eq!((num(ui, &["glyph_resets"]), num(ui, &["glyph_growths"])), (1, 0), "{}", ui.render());
    assert_eq!(win.frame_sha256.as_deref(), Some(off.frame_sha256.as_str()), "back at the first size: the same frame");
    validation::assert_clean(&off.receipt);
    validation::assert_clean(&win.receipt);
}

/// Item 8(f), AC-LOOK-04: idle Vulkan frames add no pass, instance or upload.
#[test]
fn idle_vulkan_frames_add_no_pass_instance_or_upload() {
    let s = Scratch::new("vk-idle");
    let mut f = GuiFrame::open(&fixture(&s.0), SIZE.0, SIZE.1, Some(1000)).unwrap();
    f.set_chrome(ChromeMode::Vulkan);
    let mut owner = Owner::open(OwnerConfig::default()).expect("Vulkan 1.3 device");
    let mut pass = UiPass::new(&owner).unwrap();
    let pane = owner.create_pane(SIZE.0, SIZE.1).unwrap();
    assert!(f.frame().unwrap());
    assert!(f.draw(&mut VulkanChrome { pass: &mut pass, owner: &mut owner, pane }).unwrap());
    let before = pass.stats();
    for _ in 0..10 {
        assert!(!f.frame().unwrap(), "idle frame");
        assert!(!f.draw(&mut VulkanChrome { pass: &mut pass, owner: &mut owner, pane }).unwrap(), "idle frames draw nothing");
    }
    assert_eq!(pass.stats(), before);
    assert!(f.stats().idle_frames >= 10);
    // A re-created pane starts empty: mark_upload makes the next frame draw it.
    owner.release_pane(pane).unwrap();
    let pane = owner.create_pane(SIZE.0, SIZE.1).unwrap();
    f.mark_upload();
    assert!(f.draw(&mut VulkanChrome { pass: &mut pass, owner: &mut owner, pane }).unwrap());
    assert_eq!(pass.stats().draws, before.draws + 1);
    assert_eq!(pass.stats().glyph_uploads, before.glyph_uploads, "resident glyphs stay");
    drop(pass);
    drop(owner);
    let v = hsk_studio_vulkan::process_validation();
    if v.state == "on" {
        assert_eq!((v.errors, v.warnings), (0, 0), "{v:?}");
        assert!(v.loader.iter().all(|n| hsk_studio_vulkan::is_expected_loader_note(&n.name, &n.text)), "{v:?}");
    }
}

/// Per-class maximum difference over the rects of the trace (classes as the pass names them), plus the max outside
/// every non-exact class.
fn class_maxima(want: &[u8], got: &[u8], width: u32, trace: &[hsk_studio_vulkan::ui::TraceRecord]) -> std::collections::BTreeMap<&'static str, u8> {
    let w = width as usize;
    let h = want.len() / 4 / w;
    let mut out = std::collections::BTreeMap::new();
    for r in trace {
        let (x0, x1) = (r.rect.x0.clamp(0, w as i32) as usize, r.rect.x1.clamp(0, w as i32) as usize);
        let (y0, y1) = (r.rect.y0.clamp(0, h as i32) as usize, r.rect.y1.clamp(0, h as i32) as usize);
        let mut m = 0u8;
        for y in y0..y1 {
            for x in x0..x1 {
                let i = (y * w + x) * 4;
                m = m.max((0..4).map(|k| want[i + k].abs_diff(got[i + k])).max().unwrap_or(0));
            }
        }
        let e = out.entry(r.class).or_insert(0u8);
        *e = (*e).max(m);
    }
    out
}

/// Lead ruling 2026-11-08 (after SMB-GUI-FRAME-1 landed): CPU versus Vulkan on GUI-FRAME-1's full frame at 1920 x 1080
/// and 1280 x 720 offscreen, all 11 lists of compose (menu, tabs, context, rail, colour proxy, browse, viewport with the
/// rotated ruler labels as icon masks, document, inspect, time, status). Measured maxima per class are printed; every
/// class measured 0 at both sizes.
#[test]
fn gui_frame_1_full_frame_on_the_ui_pass_matches_the_cpu_painter() {
    struct Count(usize);
    impl ChromeTarget for Count {
        fn draw(&mut self, c: Chrome<'_>) -> Result<(), HostError> {
            self.0 = c.lists.len();
            Ok(())
        }
    }
    let s = Scratch::new("vk-full");
    let doc = fixture(&s.0);
    let mut owner = Owner::open(OwnerConfig::default()).expect("Vulkan 1.3 device");
    let mut pass = UiPass::new(&owner).expect("ui pass");
    pass.set_trace(true);
    let mut report = Vec::new();
    for (w, h) in [(1920u32, 1080u32), (1280, 720)] {
        let mut f = GuiFrame::open(&doc, w, h, Some(1000)).unwrap();
        f.set_chrome(ChromeMode::Vulkan);
        assert!(f.frame().unwrap());
        let mut count = Count(0);
        f.compose(&mut count).unwrap();
        assert_eq!(count.0, 11, "compose lists");
        let pane = owner.create_pane(w, h).unwrap();
        f.mark_upload();
        assert!(f.draw(&mut VulkanChrome { pass: &mut pass, owner: &mut owner, pane }).unwrap());
        let got = owner.read_pane(pane).unwrap();
        owner.release_pane(pane).unwrap();
        let mut cpu = CpuChrome::default();
        f.compose(&mut cpu).unwrap();
        let want = cpu.canvas.unwrap().rgba().to_vec();
        let icons = pass.trace().iter().filter(|r| r.kind == "icon" && r.instances > 0).count();
        assert!(icons > 20, "designer icons and rotated ruler labels drawn: {icons}");
        let (max, n, outside) = gui::parity(&want, &got, w, pass.trace());
        let classes = class_maxima(&want, &got, w, pass.trace());
        let st = pass.stats();
        report.push(format!("{w}x{h}: max {max} differing {n} outside_aa {outside} classes {classes:?} passes {} icon_over_limit {} glyph_refusals {}", st.passes, st.icon_over_limit, st.glyph_refusals));
        assert_eq!(outside, 0, "{w}x{h}: pixels outside the AA rects differ");
        // Measured 2026-11-08 on the RTX 3090: 0 in every class (aa, flat, glyph, icon, image_translucent, scissor,
        // translucent) at both sizes; the bound is the measurement.
        assert_eq!(max, 0, "{w}x{h}: max {max} ({n} px) {classes:?}");
        assert_eq!((st.icon_over_limit, st.glyph_refusals), (0, 0));
    }
    println!("full frame parity {}", report.join("; "));
    drop(pass);
    drop(owner);
    let v = hsk_studio_vulkan::process_validation();
    if v.state == "on" {
        assert_eq!((v.errors, v.warnings), (0, 0), "{v:?}");
        assert!(v.loader.iter().all(|n| hsk_studio_vulkan::is_expected_loader_note(&n.name, &n.text)), "{v:?}");
    }
}
