//! Feature `document`: a real document through harness-view's public `Viewer::open` and render on render-cpu
//! (`Prefer::Cpu`, no viewer GPU device), shown offscreen and in a quiet Vulkan window with the same frame hash and no
//! focus change. GPU test group; validation on in debug builds when the loader lists the Khronos layer, zero errors.
//! Needs a round unit with feature `document` for hsk-studio-native-host (rule 4e).
//! Known gap: the repo's demo.handshake has no working colour profile, so the viewer refuses it
//! (`no_working_profile`, harness-view render.rs); the host reports that typed refusal and opens no window.
#![cfg(feature = "document")]
mod validation;

use hsk_studio_native_host::HostError;
use hsk_studio_native_host::capture::write_png;
use hsk_studio_native_host::offscreen::{self, OffscreenArgs};
use hsk_studio_native_host::scene::{GuiArgs, Scene, painted};
use hsk_studio_native_host::window::{self, WindowArgs};
use std::path::{Path, PathBuf};
use std::time::Duration;
use hsk_studio_accord::CancellationToken;
use hsk_studio_native_host::cli::Caller;
use hsk_studio_native_host::diag::HostRun;
use hsk_studio_observe::{DeliveryClass, DeliveryError, SinkPort};

/// One host entry as the asset-local caller (F-R60-22).
fn observed<T>(run: impl FnOnce(&mut HostRun<'_>) -> T) -> T {
    let (cancel, mut frames) = (CancellationToken::default(), Frames::default());
    let caller = Caller::asset_local();
    run(&mut HostRun::new(caller.actor, caller.resource, 1, &cancel, &mut frames).expect("host run"))
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

fn offscreen_args(path: &Path, out: Option<PathBuf>) -> OffscreenArgs {
    OffscreenArgs { width: 1, height: 1, scene: Scene::Document(path.to_path_buf()), out, receipt: None, allow_overlays: false, gui: GuiArgs::default() }
}

#[test]
fn imported_png_document_offscreen_and_quiet_window_match() {
    let dir = std::env::temp_dir().join(format!("hsk-native-host-{}-document", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("card.png");
    write_png(&source, 96, 64, &painted(96, 64).unwrap()).unwrap();

    let off = observed(|host_run| offscreen::run(&offscreen_args(&source, Some(dir.join("document-offscreen.png"))), host_run)).expect("document offscreen");
    assert!(off.exact);
    let facts = off.document.clone().expect("document facts");
    assert_eq!(facts.backend, "cpu", "Prefer::Cpu: render-cpu, no viewer GPU device");
    assert!(facts.layers >= 1, "{facts:?}");
    assert_eq!((off.width, off.height), (96, 64));
    let first = &off.rgba8[..4];
    assert!(off.rgba8.chunks_exact(4).any(|p| p != first), "the document's pixels arrived");

    let win = observed(|host_run| window::run(&WindowArgs {
        width: 1,
        height: 1,
        scene: Scene::Document(source.clone()),
        quiet: true,
        frames: 3,
        exercise: false,
        capture: Some(dir.join("document-swapchain.png")),
        receipt: Some(dir.join("document-swapchain.json")),
        allow_overlays: false,
        deadline: Duration::from_secs(60),
        gui: GuiArgs::default(),
    }, host_run))
    .expect("document window");
    assert_eq!(win.focus.focus_changes, 0, "{:?}", win.focus);
    assert_eq!(win.frame_sha256.as_deref(), Some(off.frame_sha256.as_str()));
    let receipt = std::fs::read_to_string(dir.join("document-swapchain.json")).unwrap();
    assert!(receipt.contains(r#""chrome":"cpu painter""#), "{receipt}");
    assert!(receipt.contains(r#""file_name":"card.png""#), "{receipt}");
    validation::assert_clean(&off.receipt);
    validation::assert_clean(&win.receipt);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn demo_handshake_without_working_profile_is_a_typed_refusal() {
    let demo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../hsk-studio-harness/tests/fixtures/demo.handshake");
    let err = observed(|host_run| offscreen::run(&offscreen_args(&demo, None), host_run)).err();
    assert_eq!(err, Some(HostError::Document("no_working_profile")));
}
