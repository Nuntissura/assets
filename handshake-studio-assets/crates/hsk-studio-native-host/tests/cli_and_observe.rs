//! `studio-vk` parsing (unknown input is exit 2 and never a window) and terminal outcomes through observe. No device.
use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};
use hsk_studio_native_host::cli::{Command, parse};
use hsk_studio_native_host::diag::{emit_terminal, outcome};
use hsk_studio_native_host::scene::{Scene, over_checker, painted};
use hsk_studio_native_host::HostError;
use hsk_studio_observe::{DeliveryClass, DeliveryError, FailureCode, Outcome, SinkPort};
use hsk_studio_vulkan::Refusal;

#[test]
fn unknown_or_missing_input_is_usage_exit_2() {
    for bad in [
        vec![],
        vec!["paint"],
        vec!["offscreen", "--bogus"],
        vec!["window", "--quiet"],
        vec!["window", "--quiet", "--frames", "0"],
        vec!["window", "--exercise", "--frames", "3"],
        vec!["offscreen", "--size", "0x10"],
        vec!["offscreen", "--size", "20000x10"],
        vec!["offscreen", "--size", "9000x9000"],
        vec!["offscreen", "--clear", "1,2,3"],
        vec!["offscreen", "--clear", "1,2,3,256"],
        vec!["offscreen", "--capture", "x.png"],
        vec!["window", "--out", "x.png"],
        vec!["offscreen", "--scene", "document"],
        vec!["offscreen", "--receipt"],
        vec!["window", "--frames", "100001"],
    ] {
        let err = parse(&bad).expect_err(&format!("{bad:?}"));
        assert!(matches!(err, HostError::Usage(_)), "{bad:?} -> {err:?}");
        assert_eq!(err.exit_code(), 2, "{bad:?}");
    }
    assert_eq!(parse(&["help"]), Ok(Command::Help));
    assert_eq!(parse(&["--help"]), Ok(Command::Help));
}

#[test]
fn offscreen_and_quiet_window_parse() {
    let Ok(Command::Offscreen(o)) = parse(&["offscreen", "--size", "96x64", "--clear", "1,2,3,255", "--out", "a.png"]) else {
        panic!("offscreen parse");
    };
    assert_eq!((o.width, o.height, o.scene.clone()), (96, 64, Scene::Clear([1, 2, 3, 255])));
    assert!(o.out.is_some() && !o.allow_overlays);
    let Ok(Command::Window(w)) = parse(&["window", "--quiet", "--frames", "4", "--exercise", "--scene", "painted", "--capture", "c.png"]) else {
        panic!("window parse");
    };
    assert!(w.quiet && w.exercise && w.frames == 4 && w.capture.is_some());
    assert_eq!(w.scene, Scene::Painted);
    let Ok(Command::Window(plain)) = parse(&["window"]) else { panic!("plain window") };
    assert!(!plain.quiet && plain.frames == 0);
}

#[test]
fn cpu_painter_is_deterministic_and_flattens_alpha() {
    let a = painted(40, 30).unwrap();
    assert_eq!(a, painted(40, 30).unwrap());
    assert_eq!(a.len(), 40 * 30 * 4);
    assert!(a.chunks_exact(4).all(|p| p[3] == 255));
    assert_eq!(&a[..4], &[255, 255, 255, 255], "border");
    // Transparent over the checker shows the checker; opaque stays.
    let src = [10u8, 20, 30, 0, 10, 20, 30, 255];
    assert_eq!(over_checker(2, &src).unwrap(), vec![255, 255, 255, 255, 10, 20, 30, 255]);
    assert!(over_checker(3, &src).is_err());
    assert!(painted(0, 4).is_err());
}

#[derive(Default)]
struct Sink {
    frames: Vec<(DeliveryClass, usize)>,
}

impl SinkPort for Sink {
    fn try_send(&mut self, class: DeliveryClass, bytes: &[u8]) -> Result<(), DeliveryError> {
        self.frames.push((class, bytes.len()));
        Ok(())
    }
}

#[test]
fn terminal_outcomes_go_through_observe_as_codes() {
    assert_eq!(outcome(Ok(())), Outcome::Success);
    assert_eq!(outcome(Err(HostError::Vulkan(Refusal::DeviceLost))), Outcome::Failure(FailureCode::Loss));
    assert_eq!(outcome(Err(HostError::Vulkan(Refusal::VulkanUnavailable))), Outcome::Failure(FailureCode::Unavailable));
    assert_eq!(outcome(Err(HostError::Vulkan(Refusal::SwapchainFormatUnsupported))), Outcome::Failure(FailureCode::Unsupported));
    assert_eq!(outcome(Err(HostError::FocusViolation)), Outcome::Failure(FailureCode::Validation));
    let actor = ActorContext::new("account", "principal", "owner", "owner-principal", "space", "session").unwrap();
    let resource = DomainId::parse("SDOC-0190a1b2-c3d4-7e5f-8a6b-7c8d9e0f1a2b").unwrap();
    let mut sink = Sink::default();
    let receipt = emit_terminal(&actor, &resource, 7, Err(HostError::Vulkan(Refusal::SurfaceUnrecoverable)), &CancellationToken::default(), &mut sink).unwrap();
    assert_eq!(receipt.outcome, Outcome::Failure(FailureCode::Loss));
    assert_eq!(sink.frames.len(), 1);
    assert_eq!(sink.frames[0].0, DeliveryClass::Terminal);
}
