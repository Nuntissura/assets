//! F-R60-22 (lane SMB-FIX-HOST-TERMINAL-FRAME-1): every host run sends exactly one terminal observe frame through the
//! caller's sink, on every exit path, attributed to the caller (HOST INTEGRATION (4), (5); STU-ARC-026: codes,
//! counters and opaque ids, never text). The library entries run with a collecting sink; studio-vk runs as a child
//! process on command lines it refuses before any device or window. Only the clear-scene test opens the Vulkan owner
//! (offscreen, no window; GPU test group).
mod validation;

use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};
use hsk_studio_native_host::HostError;
use hsk_studio_native_host::cli::{self, ASSET_LOCAL_RESOURCE, Caller};
use hsk_studio_native_host::diag::{HostRun, OBSERVE_LINE_SCHEMA, observe_line, outcome};
use hsk_studio_native_host::offscreen::{self, OffscreenArgs};
use hsk_studio_native_host::scene::{DEFAULT_CLEAR, GuiArgs, Scene};
use hsk_studio_native_host::window::{self, WindowArgs};
use hsk_studio_observe::{DeliveryClass, DeliveryError, Error, FailureCode, Outcome, Receipt, SinkPort};
use hsk_studio_vulkan::Refusal;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

const CORRELATION: u64 = 0x5eed_0022;
const ACTOR: &str = "acct-r60-22";
const SESSION: &str = "session-r60-22";
const RESOURCE: &str = "SDOC-0190a1b2-c3d4-7e5f-8a6b-7c8d9e0f1a2b";

/// Every frame, class and bytes.
#[derive(Default)]
struct Collect(Vec<(DeliveryClass, Vec<u8>)>);

impl SinkPort for Collect {
    fn try_send(&mut self, class: DeliveryClass, bytes: &[u8]) -> Result<(), DeliveryError> {
        self.0.push((class, bytes.to_vec()));
        Ok(())
    }
}

/// Refuses every frame.
struct Refuse;

impl SinkPort for Refuse {
    fn try_send(&mut self, _: DeliveryClass, _: &[u8]) -> Result<(), DeliveryError> {
        Err(DeliveryError::Rejected)
    }
}

fn test_caller() -> (ActorContext, DomainId) {
    (ActorContext::new(ACTOR, ACTOR, ACTOR, ACTOR, ACTOR, SESSION).unwrap(), DomainId::parse(RESOURCE).unwrap())
}

type Frames = Vec<(DeliveryClass, Vec<u8>)>;

/// Runs `entry` as the test caller: its result, the frames the sink got, and what the host says was delivered.
fn as_caller<T>(entry: impl FnOnce(&mut HostRun<'_>) -> Result<T, HostError>) -> (Result<T, HostError>, Frames, Option<Result<Receipt, Error>>) {
    let (actor, resource) = test_caller();
    let cancel = CancellationToken::default();
    let mut sink = Collect::default();
    let mut host = HostRun::new(actor, resource, CORRELATION, &cancel, &mut sink).unwrap();
    let result = entry(&mut host);
    let terminal = host.terminal();
    drop(host);
    (result, sink.0, terminal)
}

fn contains(haystack: &[u8], needle: &str) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle.as_bytes())
}

/// Exactly one frame, terminal, with `want`, the caller's correlation and the caller's ids.
fn assert_one_terminal(frames: &Frames, terminal: Option<Result<Receipt, Error>>, want: Outcome) {
    assert_eq!(frames.len(), 1, "one terminal frame per host run, got {} frames", frames.len());
    assert_eq!(frames[0].0, DeliveryClass::Terminal);
    let receipt = terminal.expect("the run sent its terminal frame").expect("delivered");
    assert_eq!((receipt.outcome, receipt.correlation_id, receipt.sequence), (want, CORRELATION, 1));
    assert_eq!(receipt.delivered_bytes, frames[0].1.len());
    for id in [ACTOR, SESSION, RESOURCE] {
        assert!(contains(&frames[0].1, id), "the caller's {id} is in the frame");
    }
}

fn clear_args(width: u32, height: u32) -> OffscreenArgs {
    OffscreenArgs { width, height, scene: Scene::Clear(DEFAULT_CLEAR), out: None, receipt: None, allow_overlays: false, gui: GuiArgs::default() }
}

fn quiet_window(scene: Scene, frames: u32) -> WindowArgs {
    WindowArgs {
        width: 64,
        height: 48,
        scene,
        quiet: true,
        frames,
        exercise: false,
        capture: None,
        receipt: None,
        allow_overlays: false,
        deadline: Duration::from_secs(30),
        gui: GuiArgs::default(),
    }
}

fn missing(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("hsk-native-host-r60-22-{}-{name}-missing.handshake", std::process::id()));
    assert!(!path.exists());
    path
}

const VALIDATION_FAILURE: Outcome = Outcome::Failure(FailureCode::Validation);

#[test]
fn offscreen_clear_scene_sends_one_success_frame() {
    let (result, frames, terminal) = as_caller(|host| offscreen::run(&clear_args(64, 48), host));
    let off = result.expect("offscreen clear run");
    assert!(off.exact);
    assert_one_terminal(&frames, terminal, Outcome::Success);
    validation::assert_clean(&off.receipt);
}

#[test]
fn offscreen_vulkan_refusal_sends_one_failure_frame() {
    // A zero extent is refused before the owner opens: no device.
    let (result, frames, terminal) = as_caller(|host| offscreen::run(&clear_args(0, 48), host));
    let err = result.err().expect("refused");
    assert_eq!(err, HostError::Vulkan(Refusal::InvalidExtent));
    assert_one_terminal(&frames, terminal, outcome(Err(err)));
    assert_eq!(outcome(Err(err)), VALIDATION_FAILURE);
}

#[test]
fn window_usage_refusal_sends_one_failure_frame_and_opens_no_window() {
    let (result, frames, terminal) = as_caller(|host| window::run(&quiet_window(Scene::Clear(DEFAULT_CLEAR), 0), host));
    assert_eq!(result.err(), Some(HostError::Usage("quiet_needs_frames")));
    assert_one_terminal(&frames, terminal, VALIDATION_FAILURE);
}

#[test]
fn document_and_gui_scene_refusals_send_one_failure_frame() {
    // A missing document is refused before any device or window (feature document: `not_found`; without it:
    // `feature_off`).
    let offscreen_doc = OffscreenArgs { scene: Scene::Document(missing("doc")), ..clear_args(64, 48) };
    let offscreen_gui = OffscreenArgs { scene: Scene::Gui(missing("gui")), ..clear_args(64, 48) };
    for args in [offscreen_doc, offscreen_gui] {
        let (result, frames, terminal) = as_caller(|host| offscreen::run(&args, host));
        assert!(matches!(result.as_ref().err(), Some(HostError::Document(_))), "{:?}: {:?}", args.scene, result.err());
        assert_one_terminal(&frames, terminal, VALIDATION_FAILURE);
    }
    for scene in [Scene::Document(missing("wdoc")), Scene::Gui(missing("wgui"))] {
        let (result, frames, terminal) = as_caller(|host| window::run(&quiet_window(scene.clone(), 1), host));
        assert!(matches!(result.as_ref().err(), Some(HostError::Document(_))), "{scene:?}: {:?}", result.err());
        assert_one_terminal(&frames, terminal, VALIDATION_FAILURE);
    }
}

#[test]
fn a_run_has_one_terminal_frame_and_a_second_finish_sends_nothing() {
    let (actor, resource) = test_caller();
    let cancel = CancellationToken::default();
    let mut sink = Collect::default();
    let mut host = HostRun::new(actor, resource, CORRELATION, &cancel, &mut sink).unwrap();
    let refused = window::run(&quiet_window(Scene::Clear(DEFAULT_CLEAR), 0), &mut host);
    assert_eq!(refused.err(), Some(HostError::Usage("quiet_needs_frames")));
    // The entry sent the frame: the host's own finish after it is refused and sends nothing.
    assert_eq!(host.finish(Ok(())), Err(Error::Closed));
    assert_eq!(host.finish(Err(HostError::Timeout)), Err(Error::Closed));
    assert_eq!(host.terminal().map(|t| t.map(|r| r.outcome)), Some(Ok(VALIDATION_FAILURE)));
    drop(host);
    assert_eq!(sink.0.len(), 1);
}

#[test]
fn a_refused_delivery_never_changes_the_run_result() {
    let (actor, resource) = test_caller();
    let cancel = CancellationToken::default();
    let mut refuse = Refuse;
    let mut host = HostRun::new(actor, resource, CORRELATION, &cancel, &mut refuse).unwrap();
    let refused = window::run(&quiet_window(Scene::Clear(DEFAULT_CLEAR), 0), &mut host);
    assert_eq!(refused.err(), Some(HostError::Usage("quiet_needs_frames")));
    assert_eq!(host.terminal(), Some(Err(Error::Delivery(DeliveryError::Rejected))));
    // One attempt per run: no retry through a later finish.
    assert_eq!(host.finish(Ok(())), Err(Error::Closed));
}

#[test]
fn observe_line_of_a_short_or_foreign_frame_names_no_header() {
    for bytes in [&b""[..], b"HSKO", b"XXXX0123456789012345678901234567890123"] {
        let line = observe_line(DeliveryClass::Terminal, bytes, false);
        for field in ["\"outcome\":null", "\"failure\":null", "\"correlation\":null", "\"sequence\":null"] {
            assert!(line.contains(field), "{line}");
        }
    }
}

// studio-vk as a child process: every command line below is refused before any device or window opens.

fn studio_vk(args: &[&str]) -> (i32, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_studio-vk")).args(args).output().expect("studio-vk runs");
    (out.status.code().expect("exit code"), String::from_utf8_lossy(&out.stderr).into_owned())
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("hsk-native-host-r60-22-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn hex(s: &str) -> String {
    s.bytes().map(|b| format!("{b:02x}")).collect()
}

fn field<'a>(line: &'a str, name: &str) -> &'a str {
    let key = format!("\"{name}\":");
    let rest = &line[line.find(&key).unwrap_or_else(|| panic!("{name} in {line}")) + key.len()..];
    let end = rest.find([',', '}']).unwrap_or(rest.len());
    rest[..end].trim_matches('"')
}

/// The one observe line of a refused run: terminal, a validation failure, `caller_default` as given, ids in the frame.
fn assert_refusal_line(line: &str, caller_default: bool, ids: &[&str]) {
    assert_eq!(field(line, "schema"), OBSERVE_LINE_SCHEMA, "{line}");
    assert_eq!(field(line, "class"), "terminal", "{line}");
    assert_eq!(field(line, "outcome"), VALIDATION_FAILURE.wire().to_string(), "{line}");
    assert_eq!(field(line, "failure"), FailureCode::Validation.wire().to_string(), "{line}");
    assert_eq!(field(line, "sequence"), "1", "{line}");
    assert_eq!(field(line, "caller_default"), caller_default.to_string(), "{line}");
    let frame = field(line, "frame_hex");
    assert_eq!(frame.len(), field(line, "bytes").parse::<usize>().unwrap() * 2, "{line}");
    for id in ids {
        assert!(frame.contains(&hex(id)), "{id} in the frame: {line}");
    }
}

fn observe_lines(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path).unwrap_or_default().lines().map(str::to_owned).collect()
}

#[test]
fn studio_vk_usage_refusal_sends_one_frame_as_the_named_caller() {
    let dir = scratch("named");
    let observe = dir.join("observe.jsonl");
    let o = observe.to_str().unwrap();
    let (code, stderr) = studio_vk(&["window", "--quiet", "--actor", ACTOR, "--session", SESSION, "--resource", RESOURCE, "--observe", o]);
    assert_eq!(code, 2, "{stderr}");
    assert!(stderr.contains("error usage/quiet_needs_frames"), "{stderr}");
    let lines = observe_lines(&observe);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_refusal_line(&lines[0], false, &[ACTOR, SESSION, RESOURCE]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn studio_vk_without_caller_flags_runs_as_the_asset_local_context() {
    let dir = scratch("default");
    let observe = dir.join("observe.jsonl");
    let (code, stderr) = studio_vk(&["window", "--quiet", "--observe", observe.to_str().unwrap()]);
    assert_eq!(code, 2, "{stderr}");
    let lines = observe_lines(&observe);
    assert_eq!(lines.len(), 1, "{lines:?}");
    let default = Caller::asset_local();
    assert_refusal_line(&lines[0], true, &[default.actor.account_id(), default.actor.session_id(), ASSET_LOCAL_RESOURCE]);
    // Without --observe the same line goes to stderr; stdout stays the receipt's.
    let (code, stderr) = studio_vk(&["window", "--quiet"]);
    assert_eq!(code, 2, "{stderr}");
    let on_stderr: Vec<&str> = stderr.lines().filter(|l| l.contains(OBSERVE_LINE_SCHEMA)).collect();
    assert_eq!(on_stderr.len(), 1, "{stderr}");
    assert_refusal_line(on_stderr[0], true, &[ASSET_LOCAL_RESOURCE]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn studio_vk_invalid_caller_flags_are_usage_and_still_send_one_frame() {
    let dir = scratch("invalid");
    // Each command line is otherwise a valid offscreen run: the caller refusal comes first, so no device opens.
    let cases: [(&[&str], &str); 6] = [
        (&["offscreen", "--actor", ACTOR, "--session", SESSION, "--resource", "not-an-id"], "resource_invalid"),
        (&["offscreen", "--actor", ACTOR], "caller_incomplete"),
        (&["offscreen", "--actor", ACTOR, "--actor", "b", "--session", SESSION, "--resource", RESOURCE], "flag_repeated"),
        (&["offscreen", "--actor", "", "--session", SESSION, "--resource", RESOURCE], "actor_invalid"),
        (&["paint"], "unknown_command"),
        (&["offscreen", "--actor"], "missing_value"),
    ];
    for (i, (args, detail)) in cases.iter().enumerate() {
        let observe = dir.join(format!("observe-{i}.jsonl"));
        let mut full: Vec<&str> = args.to_vec();
        let o = observe.to_str().unwrap().to_owned();
        // --observe goes before a trailing flag that lacks its value.
        full.insert(1.min(full.len()), "--observe");
        full.insert(2.min(full.len()), &o);
        let (code, stderr) = studio_vk(&full);
        assert_eq!(code, 2, "{full:?}: {stderr}");
        assert!(stderr.contains(&format!("error usage/{detail}")), "{full:?}: {stderr}");
        let lines = observe_lines(&observe);
        assert_eq!(lines.len(), 1, "{full:?}: {lines:?}");
        assert_refusal_line(&lines[0], true, &[ASSET_LOCAL_RESOURCE]);
    }
    // help is not a run: no frame.
    let observe = dir.join("observe-help.jsonl");
    let (code, _) = studio_vk(&["help", "--observe", observe.to_str().unwrap()]);
    assert_eq!(code, 0);
    assert!(!observe.exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn caller_flags_parse_beside_every_run_flag() {
    let args = ["offscreen", "--size", "8x8", "--actor", ACTOR, "--session", SESSION, "--resource", RESOURCE, "--observe", "o.jsonl"];
    assert!(cli::parse(&args).is_ok());
    let caller = cli::caller(&args).unwrap();
    assert!(!caller.caller_default);
    assert_eq!((caller.actor.account_id(), caller.actor.session_id(), caller.resource.as_str()), (ACTOR, SESSION, RESOURCE));
    assert_eq!(cli::observe(&args), Some(PathBuf::from("o.jsonl")));
    // A flag value that spells a caller flag is a value, as parse reads it.
    let args = ["offscreen", "--document", "--actor", "--observe", "o.jsonl"];
    assert_eq!(cli::caller(&args), Ok(Caller::asset_local()));
    assert_eq!(cli::observe(&args), Some(PathBuf::from("o.jsonl")));
}

/// The default caller is the asset-local test host's fixed context, the one `Viewer::new` runs on.
#[cfg(feature = "document")]
#[test]
fn the_default_caller_is_the_viewers_asset_local_context() {
    use hsk_studio_harness_view::view::Viewer;
    use hsk_studio_harness_view::view::render::Prefer;
    let viewer = Viewer::new(Prefer::Cpu);
    assert_eq!(&Caller::asset_local().actor, viewer.run().actor());
}
