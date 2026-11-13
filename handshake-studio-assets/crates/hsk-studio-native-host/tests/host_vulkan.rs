//! The host on the real device (GPU test group; validation on in debug builds when the loader lists the Khronos
//! layer, zero errors). Offscreen writes a PNG with no window; the quiet window never takes focus and its swapchain
//! capture hash equals the offscreen one; the exercise run resizes, minimises and restores without activating
//! anything and takes the typed swapchain paths. Each test is its own process (one device and one event loop per
//! process).
mod validation;

use hsk_studio_native_host::capture::sha256_hex;
use hsk_studio_native_host::offscreen::{self, OffscreenArgs};
use hsk_studio_native_host::scene::{DEFAULT_CLEAR, GuiArgs, Scene, painted};
use hsk_studio_native_host::window::{self, FocusReport, WindowArgs};
use std::path::PathBuf;
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

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("hsk-native-host-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn quiet(scene: Scene, w: u32, h: u32, frames: u32, exercise: bool, dir: &std::path::Path) -> WindowArgs {
    WindowArgs {
        width: w,
        height: h,
        scene,
        quiet: true,
        frames,
        exercise,
        capture: Some(dir.join("swapchain.png")),
        receipt: Some(dir.join("swapchain.json")),
        allow_overlays: false,
        deadline: Duration::from_secs(60),
        gui: GuiArgs::default(),
    }
}

/// HBR-QUIET at the end of the run, on the live window: no focus or foreground taken while the watcher sampled the
/// whole run, WS_EX_NOACTIVATE and WS_EX_TOOLWINDOW still set, and no WS_EX_APPWINDOW (no taskbar button).
fn assert_quiet(f: &FocusReport) {
    assert_eq!(f.focus_changes, 0, "{f:?}");
    assert_eq!(f.foreground_takes, 0, "{f:?}");
    assert!(f.no_activate_style && f.tool_window_style, "live style after the run: {f:?}");
    assert!(!f.app_window_style, "taskbar button: {f:?}");
    assert!(f.samples >= 50, "foreground sampled through the run: {f:?}");
}

#[test]
fn offscreen_png_then_quiet_window_same_hash_no_focus() {
    let dir = scratch("clear");
    let (w, h) = (96u32, 64u32);
    let off = observed(|host_run| offscreen::run(&OffscreenArgs {
        width: w,
        height: h,
        scene: Scene::Clear(DEFAULT_CLEAR),
        out: Some(dir.join("offscreen.png")),
        receipt: Some(dir.join("offscreen.json")),
        allow_overlays: false,
        gui: GuiArgs::default(),
    }, host_run))
    .expect("offscreen run");
    assert!(off.exact);
    assert_eq!(off.validation, validation::want(&off.receipt));
    let expected: Vec<u8> = DEFAULT_CLEAR.iter().copied().cycle().take((w * h * 4) as usize).collect();
    assert_eq!(off.frame_sha256, sha256_hex(&expected));
    assert_eq!(off.receipt.get("hwnd_created").map(|v| v.render()), Some("false".into()));
    // The PNG decodes back to the same pixels.
    let decoder = png::Decoder::new(std::io::BufReader::new(std::fs::File::open(dir.join("offscreen.png")).unwrap()));
    let mut reader = decoder.read_info().unwrap();
    let mut buf = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut buf).unwrap();
    assert_eq!((info.width, info.height), (w, h));
    assert_eq!(&buf[..info.buffer_size()], &off.rgba8[..]);
    let state = format!(r#""validation":"{}""#, validation::want(&off.receipt));
    assert!(std::fs::read_to_string(dir.join("offscreen.json")).unwrap().contains(&state));

    let win = observed(|host_run| window::run(&quiet(Scene::Clear(DEFAULT_CLEAR), w, h, 3, false, &dir), host_run)).expect("quiet window run");
    assert_quiet(&win.focus);
    let cap = win.capture.as_ref().expect("swapchain capture");
    assert_eq!((cap.width, cap.height), (w, h));
    assert_eq!(win.frame_sha256.as_deref(), Some(off.frame_sha256.as_str()), "swapchain frame equals offscreen frame");
    assert!(win.counters.frames_presented >= 3);
    assert_eq!(win.final_state, "running");
    assert!(dir.join("swapchain.png").is_file());
    let receipt = std::fs::read_to_string(dir.join("swapchain.json")).unwrap();
    assert!(receipt.contains(&state) && receipt.contains(r#""focus_changes":0"#), "{receipt}");
    validation::assert_clean(&off.receipt);
    validation::assert_clean(&win.receipt);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn quiet_exercise_resize_minimise_restore_presents_cpu_buffer() {
    let dir = scratch("exercise");
    let (w, h) = (120u32, 80u32);
    let win = observed(|host_run| window::run(&quiet(Scene::Painted, w, h, 4, true, &dir), host_run)).expect("exercise run");
    assert_quiet(&win.focus);
    assert_eq!(win.exercised, ["resize_grow", "minimize_no_activate", "restore_no_activate", "resize_back"]);
    let t = win.transitions.join(" ");
    assert!(t.contains("running:resized>stale"), "{t}");
    assert!(t.contains(">minimized"), "{t}");
    assert!(t.contains("minimized:") && t.contains(">stale"), "{t}");
    assert!(win.counters.swapchain_creates >= 3, "{:?}", win.counters);
    assert!(win.counters.skipped_minimized >= 1, "{:?}", win.counters);
    assert_eq!(win.swap_extent, (w, h));
    // The CPU-painted buffer reaches the swapchain byte for byte.
    assert_eq!(win.frame_sha256, Some(sha256_hex(&painted(w, h).unwrap())));
    validation::assert_clean(&win.receipt);
    let _ = std::fs::remove_dir_all(&dir);
}
