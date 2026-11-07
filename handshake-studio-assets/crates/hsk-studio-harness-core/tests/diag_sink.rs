//! Lane SMB-OBSERVE-DIAG-SINK-1: what ran survives the process. The sink over real files: the wire read back, hostile
//! frames and sizes, rotation and its bound, a directory that is not there, and processes that die. Children are this
//! test binary run again (`child_entry`); every child gets its diag dir on its Command, so none reaches the default.
use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};
use hsk_studio_harness_core::diag_sink::{DiagLimits, DiagSink, MAX_FILE_BYTES, MAX_FILES, MAX_LINE_BYTES, OpLine, survey};
use hsk_studio_harness_core::{ClientSessionId, Harness, RunContext, Selection, TransientCorrelation};
use hsk_studio_observe::{
    Budget, DeliveryClass, DeliveryError, DiagnosticDetail, Disposition, GeometryBudget, GeometryCounts, GeometryDetail,
    GeometryDisposition, GeometryRead, Observation, Observe, Outcome, Progress, ResourceCounts, ResourceDetail,
    ResourceDisposition, ShapingBudget, ShapingCounts, ShapingDetail, ShapingDisposition, ShapingRead, SinkPort, TextDisposition,
};
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

const FIXTURE: &[u8] = include_bytes!("../../hsk-studio-harness/tests/fixtures/demo.handshake");
const RESOURCE: &str = "SDOC-019abcde-0000-7000-8000-00000000beef";
const DECOY: &str = "PRIVATE-PROJECT-DECOY-zqxj-WOLF";
const CHILD_ENV: &str = "HSK_DIAG_SINK_TEST_CHILD";

struct Scratch(PathBuf);
impl Scratch {
    fn new(label: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("hsk-diag-sink-{}-{label}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn file_names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir).map_or(Vec::new(), |entries| entries.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect());
    names.sort();
    names
}
/// The numbers in a diag file's name, so segment 10 comes after segment 9.
fn numbers(name: &str) -> Vec<u64> {
    name.split(|c: char| !c.is_ascii_digit()).filter(|p| !p.is_empty()).map(|p| p.parse().unwrap()).collect()
}
/// Every line of every diag file, in the order of the files, parsed; a line that does not parse fails the test.
fn lines(dir: &Path) -> Vec<Value> {
    let mut out = Vec::new();
    let mut names: Vec<String> = file_names(dir).into_iter().filter(|n| n.starts_with("studio-diag-")).collect();
    names.sort_by_key(|n| numbers(n));
    for name in &names {
        for line in fs::read_to_string(dir.join(name)).unwrap().lines() {
            out.push(serde_json::from_str(line).unwrap_or_else(|e| panic!("{name}: {e}: {line}")));
        }
    }
    out
}
fn of_kind<'a>(lines: &'a [Value], kind: &str) -> Vec<&'a Value> {
    lines.iter().filter(|l| l["kind"] == kind).collect()
}
fn all_text(dir: &Path) -> String {
    file_names(dir).iter().map(|n| String::from_utf8_lossy(&fs::read(dir.join(n)).unwrap()).into_owned()).collect()
}

struct Capture(Vec<Vec<u8>>);
impl SinkPort for Capture {
    fn try_send(&mut self, _class: DeliveryClass, bytes: &[u8]) -> Result<(), DeliveryError> {
        self.0.push(bytes.to_vec());
        Ok(())
    }
}
fn observe() -> Observe {
    Observe::new(
        42,
        7,
        DomainId::parse(RESOURCE).unwrap(),
        ActorContext::new("account-a", "principal-p", "owner-o", "owner-principal-q", "space-s", "session-z").unwrap(),
        Budget::new(4, 8192).unwrap(),
    )
}
fn observation(outcome: Outcome) -> Observation<'static> {
    Observation {
        correlation_id: 42,
        revision: 7,
        outcome,
        progress: (outcome == Outcome::Progress).then(|| Progress::new(1, 2).unwrap()),
        private_project_text: Some(DECOY),
    }
}
/// One real frame of every wire version, from the real emitters.
fn frames_of_every_version() -> Vec<Vec<u8>> {
    let cancel = CancellationToken::default();
    let mut sink = Capture(Vec::new());
    observe().emit(observation(Outcome::Progress), &cancel, &mut sink).unwrap();
    let layer = DomainId::parse("SLYR-019abcde-0000-7000-8000-000000000002").unwrap();
    let address = hsk_studio_observe::DiagnosticAddress::new(hsk_studio_observe::DiagnosticTarget::Layer, hsk_studio_observe::DiagnosticProperty::Name, layer.clone()).unwrap();
    let detail = DiagnosticDetail::new(Disposition::Changed, None, Some(address), [1, 2, 1, 2, 2]).unwrap();
    observe().emit_detail(observation(Outcome::Success), &detail, &cancel, &mut sink).unwrap();
    let tile = hsk_studio_observe::TileAddress::new(layer.clone(), "key-1", -3, 4).unwrap();
    let counts = ResourceCounts::new([3, 2, 1], 8, [10, 20, 30], 100).unwrap();
    let resource = ResourceDetail::new(ResourceDisposition::Changed, None, Some(tile), 9, counts).unwrap();
    observe().emit_resource(observation(Outcome::Success), &resource, &cancel, &mut sink).unwrap();
    let budget = GeometryBudget { count_limit: 100, work_limit: 100, byte_limit: 100, delivery_limit: 5 };
    let counts = GeometryCounts::new([1, 2, 3, 4, 5, 6, 7, 8], [9, 10, 8], 1, budget).unwrap();
    let read = GeometryRead { source_path_id: None, read_revision: Some(11), target_revision: None, cancel_epoch: Some(13) };
    let address = hsk_studio_observe::GeometryAddress::new(&layer, "key-2").unwrap();
    let geometry = GeometryDetail::new(GeometryDisposition::Accepted, None, Some(address), read, counts).unwrap();
    observe().emit_geometry(observation(Outcome::Success), &geometry, &cancel, &mut sink).unwrap();
    let budget = ShapingBudget { counts: [100; 8], byte_limit: 100, delivery_limit: 5 };
    let counts = ShapingCounts::new([1, 2, 3, 4, 5, 6, 7, 8], [9, 10, 8], 1, budget).unwrap();
    let read = ShapingRead { source_revision: Some(21), resolver_revision: None, cancel_epoch: None, target_revision: Some(24) };
    let shaping = ShapingDetail::new(ShapingDisposition::Accepted, None, Some(TextDisposition::Shaped), None, read, counts).unwrap();
    observe().emit_shaping(observation(Outcome::Success), &shaping, &cancel, &mut sink).unwrap();
    sink.0
}

#[test]
fn every_wire_version_is_read_back_as_named_ids_codes_and_counters() {
    let scratch = Scratch::new("wire");
    let sink = DiagSink::open(&scratch.0);
    let frames = frames_of_every_version();
    for (i, frame) in frames.iter().enumerate() {
        sink.record_frame(if i == 0 { DeliveryClass::Progress } else { DeliveryClass::Terminal }, frame);
    }
    assert_eq!(sink.state().status, "active");
    let all = lines(&scratch.0);
    assert!(of_kind(&all, "frame_refused").is_empty(), "{all:?}");
    let read = of_kind(&all, "frame");
    assert_eq!(read.iter().map(|f| f["wire"].as_u64().unwrap()).collect::<Vec<_>>(), [1, 2, 3, 4, 5]);
    for (frame, bytes) in read.iter().zip(&frames) {
        assert_eq!((frame["correlation_id"].as_u64(), frame["revision"].as_u64(), frame["sequence_id"].as_u64()), (Some(42), Some(7), Some(1)));
        assert_eq!((frame["counter_a"].as_u64(), frame["counter_b"].as_u64()), (Some(bytes.len() as u64), Some(7)));
        assert_eq!(frame["resource_id"], RESOURCE);
        assert_eq!((&frame["account_id"], &frame["session_id"]), (&Value::from("account-a"), &Value::from("session-z")));
    }
    assert_eq!((read[0]["code"].as_str(), read[0]["event_code"].as_u64(), read[0]["class"].as_str()), (Some("Progress"), Some(0x5300), Some("progress")));
    assert_eq!((read[0]["progress_completed"].as_u64(), read[0]["progress_total"].as_u64()), (Some(1), Some(2)));
    assert_eq!((read[1]["code"].as_str(), read[1]["event_code"].as_u64(), read[1]["phase_marker"].as_u64()), (Some("Success"), Some(0x5301), Some(2)));
    let d = &read[1]["detail"];
    assert_eq!((d["attempted_writes"].as_u64(), d["changed_writes"].as_u64(), d["address"]["target"].as_u64()), (Some(2), Some(2), Some(1)));
    let d = &read[2]["detail"];
    assert_eq!((d["epoch"].as_u64(), d["tiles_changed"].as_u64(), d["byte_limit"].as_u64()), (Some(9), Some(1), Some(100)));
    assert_eq!((d["address"]["object_key"].as_str(), d["address"]["column"].as_i64(), d["address"]["row"].as_i64()), (Some("key-1"), Some(-3), Some(4)));
    let d = &read[3]["detail"];
    assert_eq!((d["work_units"].as_u64(), d["retained_owned"].as_u64(), d["delivery_limit"].as_u64()), (Some(8), Some(8), Some(5)));
    assert_eq!((d["read_revision"].as_u64(), d["cancel_epoch"].as_u64(), d["target_revision"].as_u64()), (Some(11), Some(13), None));
    assert_eq!(d["address"]["object_key"], "key-2");
    let d = &read[4]["detail"];
    assert_eq!((d["result"].as_u64(), d["glyphs"].as_u64(), d["limit_glyphs"].as_u64()), (Some(1), Some(5), Some(100)));
    assert_eq!((d["source_revision"].as_u64(), d["target_revision"].as_u64()), (Some(21), Some(24)));
    assert!(!all_text(&scratch.0).contains(DECOY), "the observation's private text is never in a frame");
}

/// Every prefix of every real frame, and every frame with one more byte: refused with a reason, never a panic, and none
/// of its bytes are written.
#[test]
fn truncated_and_trailing_frames_are_refused_with_none_of_their_bytes() {
    let scratch = Scratch::new("truncated");
    let sink = DiagSink::open_with(&scratch.0, DiagLimits::new(1024 * 1024, 8));
    let frames = frames_of_every_version();
    let mut expected = 0;
    for frame in &frames {
        for cut in 0..frame.len() {
            sink.record_frame(DeliveryClass::Terminal, &frame[..cut]);
            expected += 1;
        }
        let mut longer = frame.clone();
        longer.push(0x41);
        sink.record_frame(DeliveryClass::Terminal, &longer);
        expected += 1;
    }
    let all = lines(&scratch.0);
    let refused = of_kind(&all, "frame_refused");
    assert!(of_kind(&all, "frame").is_empty());
    assert_eq!(refused.len(), expected);
    assert!(refused.iter().all(|r| matches!(r["reason"].as_str(), Some("truncated" | "bad_magic" | "trailing_bytes"))), "{refused:?}");
    assert!(!all_text(&scratch.0).contains("account-a"), "a refused frame's text is not written");
}

fn frame_with_string_length(length: u16) -> Vec<u8> {
    let mut frame = b"HSKO\x01\x02\x00\x00".to_vec();
    frame.extend_from_slice(&[0; 40]);
    frame.extend_from_slice(&length.to_be_bytes());
    frame.extend_from_slice(DECOY.as_bytes());
    frame
}

/// V4-02: a length field at its maximum, and a frame over the wire's own budget before anything is read.
#[test]
fn string_length_u16_max_and_frame_over_budget_are_refused() {
    let scratch = Scratch::new("hostile");
    let sink = DiagSink::open(&scratch.0);
    sink.record_frame(DeliveryClass::Terminal, &frame_with_string_length(u16::MAX));
    let mut huge = frames_of_every_version().remove(0);
    huge.resize(64 * 1024 * 1024, b'x');
    sink.record_frame(DeliveryClass::Terminal, &huge);
    drop(huge);
    let mut other_version = frames_of_every_version().remove(0);
    other_version[4] = 9;
    sink.record_frame(DeliveryClass::Terminal, &other_version);
    let mut magic = frames_of_every_version().remove(0);
    magic[0] = b'X';
    sink.record_frame(DeliveryClass::Terminal, &magic);
    let mut not_utf8 = frame_with_string_length(4);
    not_utf8.truncate(50);
    not_utf8.extend_from_slice(&[0xFF, 0xFE, 0xFD, 0xFC]);
    sink.record_frame(DeliveryClass::Terminal, &not_utf8);
    let all = lines(&scratch.0);
    let reasons: Vec<&str> = of_kind(&all, "frame_refused").iter().map(|r| r["reason"].as_str().unwrap()).collect();
    assert_eq!(reasons, ["truncated", "frame_limit", "bad_version", "bad_magic", "not_utf8"]);
    assert_eq!(of_kind(&all, "frame_refused")[1]["len"].as_u64(), Some(64 * 1024 * 1024));
    let text = all_text(&scratch.0);
    assert!(!text.contains(DECOY) && text.len() < 64 * 1024, "{} bytes", text.len());
}

/// A very long or text-bearing name never reaches a line: the field is null, the line is bounded.
#[test]
fn a_very_long_detail_is_refused_and_the_line_stays_bounded() {
    let scratch = Scratch::new("long");
    let sink = DiagSink::open(&scratch.0);
    let long = format!("{DECOY}{}", "x".repeat(4 * 1024 * 1024));
    let path_like = format!("/home/someone/{DECOY}.handshake");
    sink.record_op(&OpLine { op: &long, n: 1, ok: false, revision: None, frames: 0, error: Some(&path_like), correlation: None, command_id: Some(&long) });
    sink.record_run_start(&path_like);
    let all = lines(&scratch.0);
    let op = of_kind(&all, "op")[0];
    assert!(op["op"].is_null() && op["error"].is_null() && op["command_id"].is_null(), "{op}");
    assert!(of_kind(&all, "run_start")[0]["command"].is_null());
    let text = all_text(&scratch.0);
    assert!(!text.contains(DECOY) && text.lines().all(|l| l.len() < MAX_LINE_BYTES));
    // The real wire at its largest: seven strings of control characters, each escaped to six bytes, go over the line
    // limit, and the line is refused whole.
    let mut wide = b"HSKO\x01\x02\x00\x00".to_vec();
    wide.extend_from_slice(&[0; 40]);
    for _ in 0..7 {
        wide.extend_from_slice(&280u16.to_be_bytes());
        wide.extend_from_slice(&[0x01; 280]);
    }
    assert!(wide.len() <= hsk_studio_observe::MAX_FRAME_BYTES);
    sink.record_frame(DeliveryClass::Terminal, &wide);
    let all = lines(&scratch.0);
    assert_eq!(of_kind(&all, "frame_refused").last().unwrap()["reason"], "line_limit");
    assert!(all_text(&scratch.0).lines().all(|l| l.len() < MAX_LINE_BYTES));
}

#[test]
fn rotation_keeps_the_directory_inside_its_bound_and_removes_only_our_names() {
    let scratch = Scratch::new("rotation");
    // Not ours, whatever they look like; each must survive every rotation.
    let foreign = ["notes.jsonl", "studio-diag-1-2-3.jsonl", "studio-diag-a-2-3-4.jsonl", "studio-diag-1-2-3-4.jsonl.bak", "studio-panic-1.jsonl"];
    for name in foreign {
        fs::write(scratch.0.join(name), DECOY).unwrap();
    }
    // Ours from an older process: the oldest numbers, so these go first.
    let older = "studio-diag-1-2-3-4.jsonl";
    fs::write(scratch.0.join(older), b"{}\n").unwrap();
    let older_panic = "studio-panic-1-2.jsonl";
    fs::write(scratch.0.join(older_panic), b"{}\n").unwrap();

    let limits = DiagLimits::new(32 * 1024, 4);
    let sink = DiagSink::open_with(&scratch.0, limits);
    let frame = frames_of_every_version().remove(1);
    let per_run = 2000;
    for _ in 0..per_run {
        sink.record_frame(DeliveryClass::Terminal, &frame);
        let ours: Vec<String> = file_names(&scratch.0).into_iter().filter(|n| !foreign.contains(&n.as_str())).collect();
        assert!(ours.len() <= limits.max_files(), "{ours:?}");
    }
    let state = sink.state();
    assert_eq!(state.status, "active");
    assert!(state.rotations >= 8 && state.files_removed >= 8, "{state:?}");
    assert_eq!(state.lines_dropped, 0, "{state:?}");
    for name in foreign {
        assert_eq!(fs::read_to_string(scratch.0.join(name)).unwrap(), DECOY, "{name}");
    }
    let names = file_names(&scratch.0);
    assert!(!names.iter().any(|n| n == older || n == older_panic), "the oldest of ours go first: {names:?}");
    let ours: Vec<String> = names.into_iter().filter(|n| !foreign.contains(&n.as_str())).collect();
    let total: u64 = ours.iter().map(|n| fs::metadata(scratch.0.join(n)).unwrap().len()).sum();
    assert!(total <= limits.max_file_bytes() * limits.max_files() as u64, "{total}");
    for name in &ours {
        assert!(fs::metadata(scratch.0.join(name)).unwrap().len() <= limits.max_file_bytes(), "{name}");
    }
    // What is kept is the newest and in order: seq only rises across the kept files, and the last frame is the last line.
    let all = lines(&scratch.0);
    let seqs: Vec<u64> = all.iter().filter(|l| l["kind"] != "panic").map(|l| l["seq"].as_u64().unwrap_or(0)).filter(|s| *s > 0).collect();
    assert!(seqs.windows(2).all(|w| w[0] < w[1]), "{:?}", &seqs[..seqs.len().min(20)]);
    assert_eq!(all.last().unwrap()["kind"], "frame");
    assert!(of_kind(&all, "segment_end").len() >= 2);
    assert_eq!((MAX_FILE_BYTES, MAX_FILES), (4 * 1024 * 1024, 16), "the bounds the manual states");
}

/// A path that cannot be a directory stands in for a read-only location on every platform.
#[test]
fn a_dir_that_cannot_be_used_never_fails_the_caller_and_stops_touching_it() {
    let scratch = Scratch::new("unusable");
    let not_a_dir = scratch.0.join("a-file");
    fs::write(&not_a_dir, b"x").unwrap();
    let sink = DiagSink::open(&not_a_dir);
    assert_eq!((sink.state().status, sink.state().code), ("unavailable", Some("dir_unavailable")));
    let frame = frames_of_every_version().remove(0);
    for _ in 0..100 {
        sink.record_frame(DeliveryClass::Progress, &frame);
        sink.record_run_end("edit", 0);
    }
    let state = sink.state();
    assert_eq!((state.status, state.code), ("stopped", Some("dir_unavailable")));
    assert_eq!((state.lines_written, state.lines_dropped), (0, 200));
    assert_eq!(fs::read(&not_a_dir).unwrap(), b"x");
    assert_eq!(file_names(&scratch.0), ["a-file"]);
}

/// A directory with the read-only attribute: on Unix the open fails; on Windows the attribute does not stop it.
#[test]
fn a_read_only_location_never_fails_the_caller() {
    let scratch = Scratch::new("readonly");
    let dir = scratch.0.join("ro");
    fs::create_dir_all(&dir).unwrap();
    let mut permissions = fs::metadata(&dir).unwrap().permissions();
    permissions.set_readonly(true);
    fs::set_permissions(&dir, permissions).unwrap();
    let sink = DiagSink::open(&dir);
    sink.record_run_start("edit");
    let state = sink.state();
    // On Windows the attribute does not stop a file from being created in a directory.
    if state.status == "active" {
        println!("NOT_PROVEN: the read-only attribute did not stop writes on this platform; a_dir_that_cannot_be_used_never_fails_the_caller_and_stops_touching_it holds the claim");
    }
    drop(sink);
    let mut permissions = fs::metadata(&dir).unwrap().permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    permissions.set_readonly(false);
    fs::set_permissions(&dir, permissions).unwrap();
}

#[test]
fn a_dir_removed_while_running_comes_back_and_never_fails_the_caller() {
    let scratch = Scratch::new("removed");
    let dir = scratch.0.join("diag");
    let sink = DiagSink::open(&dir);
    sink.record_run_start("edit");
    assert_eq!(lines(&dir).len(), 2);
    if fs::remove_dir_all(&dir).is_err() {
        println!("NOT_PROVEN: this platform refused to remove a directory holding an open file");
        return;
    }
    assert!(!dir.exists());
    let frame = frames_of_every_version().remove(0);
    for _ in 0..200 {
        sink.record_frame(DeliveryClass::Progress, &frame);
    }
    sink.record_run_end("edit", 0);
    let state = sink.state();
    assert_eq!(state.status, "active", "{state:?}");
    let all = lines(&dir);
    assert_eq!(all.last().unwrap()["kind"], "run_end");
    assert!(all.len() > 100, "{}", all.len());
}

fn context() -> RunContext {
    let actor = ActorContext::new("account-a", "principal-p", "owner-o", "owner-principal-q", "space-s", "session-z").unwrap();
    let session = ClientSessionId::parse("client-window").unwrap();
    RunContext::new(actor, session, Arc::new(TransientCorrelation::starting_at(1000)))
}

/// The run's FrameSink passes every frame on, and every op is recorded, in order; the op records keep their counters.
#[test]
fn a_run_forwards_its_frames_and_ops_to_the_sink_in_order() {
    let scratch = Scratch::new("run");
    let sink = DiagSink::open(&scratch.0);
    let harness = Harness::new(Selection::default()).unwrap().with_diag(sink.clone());
    assert!(harness.diag().is_some());
    let cancel = CancellationToken::default();
    let mut run = harness.run(context(), &cancel);
    assert!(run.open_bytes(FIXTURE));
    run.summary();
    assert!(!run.open_bytes(b"not a package"));
    let report = run.finish("test");
    let ops = report["ops"].as_array().unwrap();
    let all = lines(&scratch.0);
    let written = of_kind(&all, "op");
    assert_eq!(written.len(), ops.len());
    for (line, op) in written.iter().zip(ops) {
        assert_eq!((&line["op"], &line["n"], &line["ok"], &line["revision"]), (&op["op"], &op["n"], &op["ok"], &op["revision"]));
        assert_eq!(line["frames"], op["observe"]["frames"]);
        assert_eq!(line["correlation_id"], op["correlation"]);
        assert_eq!(line["error"], if op["ok"] == true { Value::Null } else { op["error"].clone() });
    }
    assert!(written.last().unwrap()["error"].is_string(), "{:?}", written.last());
    let frames = of_kind(&all, "frame");
    assert_eq!(frames.len() as u64, report["observe"]["frames_total"].as_u64().unwrap());
    assert!(frames.len() >= 2 && of_kind(&all, "frame_refused").is_empty(), "{all:?}");
    // In order: each op's frames come before its op line, and seq only rises.
    let seqs: Vec<u64> = all.iter().map(|l| l["seq"].as_u64().unwrap()).collect();
    assert!(seqs.windows(2).all(|w| w[0] < w[1]));
    assert_eq!(all[1]["kind"], "frame", "{:?}", all[1]);
    assert_eq!(all[2]["kind"], "op", "{:?}", all[2]);
    assert_eq!(all[1]["revision"], all[2]["revision"]);
}

/// A run with no sink behaves as before, and keeps nothing.
#[test]
fn a_harness_has_no_sink_unless_a_host_gives_it_one() {
    let harness = Harness::new(Selection::default()).unwrap();
    assert!(harness.diag().is_none());
}

// ----- processes that die -----------------------------------------------------------------------------------------

const CHILD_DECOY_PANIC: &str = "PANIC-MESSAGE-DECOY-zqxj";

#[inline(never)]
fn overflow(depth: u64) -> u64 {
    let pad = std::hint::black_box([depth; 64]);
    if depth == u64::MAX { pad[0] } else { overflow(depth + 1).wrapping_add(pad[63]) }
}

/// Not a test of its own: the body of a child process, chosen by the variable. Without it this does nothing.
#[test]
fn child_entry() {
    let Ok(spec) = std::env::var(CHILD_ENV) else { return };
    let (mode, dir) = spec.split_once(':').unwrap();
    let sink = hsk_studio_harness_core::diag_sink::start_process(Some(PathBuf::from(dir)));
    sink.record_run_start("edit");
    sink.record_op(&OpLine { op: "open", n: 1, ok: true, revision: Some(9), frames: 1, error: None, correlation: Some(1000), command_id: None });
    match mode {
        "clean" => sink.record_run_end("edit", 0),
        "panic" => panic!("{CHILD_DECOY_PANIC}"),
        "abort" => std::process::abort(),
        "overflow" => {
            let thread = std::thread::Builder::new().stack_size(256 * 1024).spawn(|| overflow(0)).unwrap();
            let _ = thread.join();
        }
        other => panic!("unknown mode {other}"),
    }
}

struct Child {
    code: Option<i32>,
    stderr: String,
}
fn run_child(mode: &str, dir: &Path) -> Child {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", "child_entry", "--test-threads=1", "--nocapture"])
        .env(CHILD_ENV, format!("{mode}:{}", dir.display()))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let mut child = command.spawn().unwrap();
    let mut stderr_pipe = child.stderr.take().unwrap();
    let reader = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = std::io::Read::read_to_string(&mut stderr_pipe, &mut text);
        text
    });
    let deadline = Instant::now() + Duration::from_secs(60);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("child {mode} did not end in 60 s");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    Child { code: status.code(), stderr: reader.join().unwrap() }
}

fn assert_ran_in_order(all: &[Value]) {
    let kinds: Vec<&str> = all.iter().filter(|l| l["kind"] != "panic").map(|l| l["kind"].as_str().unwrap()).collect();
    assert_eq!(kinds[..3], ["segment_start", "run_start", "op"], "{all:?}");
    assert_eq!(of_kind(all, "run_start")[0]["command"], "edit");
    let op = of_kind(all, "op")[0];
    assert_eq!((op["op"].as_str(), op["revision"].as_u64(), op["frames"].as_u64(), op["correlation_id"].as_u64()), (Some("open"), Some(9), Some(1), Some(1000)));
}

/// The red proof of the lane: after the process has exited, the file shows what ran, in order, with codes and counters.
#[test]
fn after_a_process_exits_its_file_shows_what_ran() {
    let scratch = Scratch::new("clean");
    let child = run_child("clean", &scratch.0);
    assert_eq!(child.code, Some(0), "{}", child.stderr);
    let all = lines(&scratch.0);
    assert_ran_in_order(&all);
    let end = all.last().unwrap();
    assert_eq!((end["kind"].as_str(), end["command"].as_str(), end["exit"].as_i64()), (Some("run_end"), Some("edit"), Some(0)));
    let start = &all[0];
    assert_eq!(start["schema"], "hsk.studio.diag@1");
    assert!(start["process_id"].as_u64().unwrap() > 0 && start["process_birth_observed_unix_ms"].as_u64().unwrap() > 0);
    let seen = survey(&scratch.0, None);
    assert_eq!((seen.files, seen.unterminated_runs, seen.panic_files, seen.runs_surveyed), (1, 0, 0, 1));
}

#[test]
fn a_panic_leaves_a_typed_record_and_keeps_the_exit_and_the_message_on_stderr() {
    let scratch = Scratch::new("panic");
    let child = run_child("panic", &scratch.0);
    // 101: the panic takes the test harness's own failure exit, as it does without the hook.
    assert_eq!(child.code, Some(101), "{}", child.stderr);
    assert!(child.stderr.contains(CHILD_DECOY_PANIC), "the previous hook still prints: {}", child.stderr);
    let names = file_names(&scratch.0);
    let panic_file = names.iter().find(|n| n.starts_with("studio-panic-")).unwrap_or_else(|| panic!("{names:?}"));
    let text = fs::read_to_string(scratch.0.join(panic_file)).unwrap();
    let record: Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
    assert_eq!((record["kind"].as_str(), record["schema"].as_str(), record["payload_kind"].as_str()), (Some("panic"), Some("hsk.studio.diag@1"), Some(record["payload_kind"].as_str().filter(|k| matches!(*k, "str" | "string")).unwrap())));
    assert_eq!(record["payload_len"].as_u64(), Some(CHILD_DECOY_PANIC.len() as u64));
    assert_eq!(record["source"], "hsk-studio-harness-core/tests/diag_sink.rs");
    assert!(record["line"].as_u64().unwrap() > 0 && record["process_id"].as_u64().unwrap() > 0);
    assert!(!all_text(&scratch.0).contains(CHILD_DECOY_PANIC), "the panic message is never written");
    let all = lines(&scratch.0);
    assert_ran_in_order(&all);
    assert_eq!(all.last().unwrap()["kind"], "panic", "the run's own stream shows it in order: {all:?}");
    let seen = survey(&scratch.0, None);
    assert_eq!((seen.panic_files, seen.unterminated_runs), (1, 1));
}

fn assert_died_without_a_record(child: &Child, dir: &Path) {
    assert_ne!(child.code, Some(0), "{}", child.stderr);
    assert!(!file_names(dir).iter().any(|n| n.starts_with("studio-panic-")), "no hook runs: {:?}", file_names(dir));
    let all = lines(dir);
    assert_ran_in_order(&all);
    assert_eq!(all.last().unwrap()["kind"], "op", "every line up to the last one written, no end of run: {all:?}");
    let seen = survey(dir, None);
    assert_eq!((seen.panic_files, seen.unterminated_runs, seen.runs_surveyed), (0, 1, 1));
}

/// What a stack overflow leaves behind, measured: no hook sees it, so there is no record of it. Every line written so
/// far is there, and the missing `run_end` is how the next `health` counts it.
#[test]
fn a_stack_overflow_leaves_the_lines_written_and_no_end_of_run() {
    let scratch = Scratch::new("overflow");
    let child = run_child("overflow", &scratch.0);
    assert_died_without_a_record(&child, &scratch.0);
    if cfg!(windows) {
        // STATUS_STACK_OVERFLOW, as F-R56-1 measured: a real overflow, not some other way to die.
        assert_eq!(child.code.map(|c| c as u32), Some(0xC000_00FD), "{}", child.stderr);
    }
}

#[test]
fn an_abort_leaves_the_lines_written_and_no_end_of_run() {
    let scratch = Scratch::new("abort");
    let child = run_child("abort", &scratch.0);
    assert_died_without_a_record(&child, &scratch.0);
}
