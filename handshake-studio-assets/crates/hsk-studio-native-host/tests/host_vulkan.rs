//! The host on the real device (GPU test group; validation: absent). Offscreen writes a PNG with no window; the quiet
//! window never takes focus and its swapchain capture hash equals the offscreen one; the exercise run resizes,
//! minimises and restores without activating anything and takes the typed swapchain paths. Each test is its own
//! process (one device and one event loop per process).
use hsk_studio_native_host::capture::sha256_hex;
use hsk_studio_native_host::offscreen::{self, OffscreenArgs};
use hsk_studio_native_host::scene::{DEFAULT_CLEAR, Scene, painted};
use hsk_studio_native_host::window::{self, WindowArgs};
use std::path::PathBuf;
use std::time::Duration;

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
    }
}

#[test]
fn offscreen_png_then_quiet_window_same_hash_no_focus() {
    let dir = scratch("clear");
    let (w, h) = (96u32, 64u32);
    let off = offscreen::run(&OffscreenArgs {
        width: w,
        height: h,
        scene: Scene::Clear(DEFAULT_CLEAR),
        out: Some(dir.join("offscreen.png")),
        receipt: Some(dir.join("offscreen.json")),
        allow_overlays: false,
    })
    .expect("offscreen run");
    assert!(off.exact);
    assert_eq!(off.validation, "absent");
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
    assert!(std::fs::read_to_string(dir.join("offscreen.json")).unwrap().contains(r#""validation":"absent""#));

    let win = window::run(&quiet(Scene::Clear(DEFAULT_CLEAR), w, h, 3, false, &dir)).expect("quiet window run");
    assert_eq!(win.focus.focus_changes, 0, "{:?}", win.focus);
    assert!(win.focus.no_activate_style && win.focus.tool_window_style);
    let cap = win.capture.as_ref().expect("swapchain capture");
    assert_eq!((cap.width, cap.height), (w, h));
    assert_eq!(win.frame_sha256.as_deref(), Some(off.frame_sha256.as_str()), "swapchain frame equals offscreen frame");
    assert!(win.counters.frames_presented >= 3);
    assert_eq!(win.final_state, "running");
    assert!(dir.join("swapchain.png").is_file());
    let receipt = std::fs::read_to_string(dir.join("swapchain.json")).unwrap();
    assert!(receipt.contains(r#""validation":"absent""#) && receipt.contains(r#""focus_changes":0"#), "{receipt}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn quiet_exercise_resize_minimise_restore_presents_cpu_buffer() {
    let dir = scratch("exercise");
    let (w, h) = (120u32, 80u32);
    let win = window::run(&quiet(Scene::Painted, w, h, 4, true, &dir)).expect("exercise run");
    assert_eq!(win.focus.focus_changes, 0, "{:?}", win.focus);
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
    let _ = std::fs::remove_dir_all(&dir);
}
