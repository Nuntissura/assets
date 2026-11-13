//! Lane SMB-FIX-CAPTURE-ADMISSION-1 (R60 attempt 3 probe `r60a3_j2_prototype_capture_frame_asks_the_bound_job_port_before_writing`,
//! class export-job-not-admitted-jobport; common lead ruling 2; STU-ARC-008, STU-ARC-013, STU-PRO-138/139):
//! `prototype.capture_frame` with a path admits its PNG write through the viewer's bound JobPort before the first byte,
//! after the cheap checks (`parameter_conflict`, the path, `overwrite_required`), and settles the job once. With no
//! document open the job names the play session's derived resource (never resource-less, lead ruling 2026-11-12);
//! with one open it names the open document. Without a path nothing is written and nothing is admitted.
#![cfg(feature = "viewer")]

use hsk_studio_harness::view::Viewer;
use hsk_studio_harness::view::render::Prefer;
use hsk_studio_session::{Action, Cost, JobPort, JobRefused, JobScope, JobTicket};
use serde_json::{Map, Value, json};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// Counts asks and releases and records each asked scope; refuses every ask when `saturated`.
#[derive(Default)]
struct Port {
    saturated: bool,
    asks: AtomicU64,
    released: AtomicU64,
    scopes: Mutex<Vec<(String, Action)>>,
}

impl Port {
    fn saturated() -> Arc<Self> {
        Arc::new(Self { saturated: true, ..Self::default() })
    }
    fn open() -> Arc<Self> {
        Arc::new(Self::default())
    }
    fn asks(&self) -> u64 {
        self.asks.load(Ordering::SeqCst)
    }
    fn released(&self) -> u64 {
        self.released.load(Ordering::SeqCst)
    }
    fn scopes(&self) -> Vec<(String, Action)> {
        self.scopes.lock().unwrap().clone()
    }
}

impl JobPort for Port {
    fn admit(&self, scope: &JobScope<'_>, _cost: Cost) -> Result<JobTicket, JobRefused> {
        let n = self.asks.fetch_add(1, Ordering::SeqCst);
        self.scopes.lock().unwrap().push((scope.resource.as_str().to_string(), scope.action));
        if self.saturated { Err(JobRefused { retry_after_ms: 5 }) } else { Ok(JobTicket(n.saturating_add(1))) }
    }
    fn release(&self, _ticket: JobTicket) {
        self.released.fetch_add(1, Ordering::SeqCst);
    }
}

/// A scratch folder of its own per test (process id and test name), removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("fix-capture-admission-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn fixture(name: &str) -> PathBuf {
    // The Operator fixtures live with the importer (repository-relative, never machine-local).
    PathBuf::from(format!("{}/../hsk-studio-interop-fig/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR")))
}

fn obj(v: Value) -> Map<String, Value> {
    match v {
        Value::Object(m) => m,
        other => panic!("not an object: {other}"),
    }
}

/// A viewer bound to `port` with the prototype fixture's play session started.
fn session(port: &Arc<Port>) -> Viewer {
    let mut v = Viewer::new(Prefer::Cpu);
    v.set_job_port(port.clone());
    let pages = fixture("fig-prototype-pages.fig");
    v.run_command("prototype.start_session", &obj(json!({"path": pages.to_str().unwrap()}))).expect("the fixture opens");
    v
}

fn capture(v: &mut Viewer, params: Value) -> Result<Value, (String, Value)> {
    v.run_command("prototype.capture_frame", &obj(params)).map_err(|r| (r.code, r.detail))
}

/// The probe's case: a saturated port refuses the capture typed `overloaded` with its retry hint, asked exactly once,
/// and no file is written; the refused job names a resource (an `SDOC` id: never resource-less) and the export action.
#[test]
fn a_saturated_port_refuses_the_capture_overloaded_and_nothing_is_written() {
    let scratch = Scratch::new("saturated");
    let port = Port::saturated();
    let mut v = session(&port);
    let frame = scratch.path("frame.png");
    let (code, detail) = capture(&mut v, json!({"path": frame.to_str().unwrap()})).expect_err("refused");
    assert_eq!(code, "overloaded", "{detail}");
    assert!(detail.to_string().contains("retry_after_ms"), "the port's retry hint: {detail}");
    assert!(!frame.exists(), "no file is written");
    assert_eq!(port.asks(), 1, "the bound port is asked exactly once");
    let scopes = port.scopes();
    assert!(scopes[0].0.starts_with("SDOC-"), "a derived document resource: {scopes:?}");
    assert_eq!(scopes[0].1, Action::Export);
    let counts = v.job_counts();
    assert_eq!((counts.refused_overloaded, counts.admitted, counts.in_flight), (1, 0, 0), "{counts:?}");
}

/// An open port admits the capture once before the write; the PNG is written, the ticket released and the job
/// settled once `completed`, which the reply carries as `job`.
#[test]
fn an_open_port_admits_once_writes_the_png_and_settles_once() {
    let scratch = Scratch::new("open");
    let port = Port::open();
    let mut v = session(&port);
    let frame = scratch.path("frame.png");
    let reply = capture(&mut v, json!({"path": frame.to_str().unwrap()})).expect("captured");
    assert!(std::fs::read(&frame).unwrap().starts_with(b"\x89PNG"), "a PNG was written");
    assert!(reply["screen"]["width"].as_u64().is_some_and(|w| w > 0), "{reply}");
    assert_eq!(reply["open"], true, "{reply}");
    assert_eq!(reply["job"]["outcome"], "completed", "{reply}");
    assert_eq!(port.asks(), 1, "asked once");
    assert_eq!(port.released(), 1, "the ticket is released");
    let counts = v.job_counts();
    assert_eq!((counts.admitted, counts.completed, counts.failed, counts.released, counts.in_flight), (1, 1, 0, 1, 0), "settled once: {counts:?}");
    // An overwrite of the written frame is one more admitted job.
    let again = capture(&mut v, json!({"path": frame.to_str().unwrap(), "overwrite": true})).expect("overwritten");
    assert_eq!(again["job"]["outcome"], "completed", "{again}");
    assert_eq!((port.asks(), port.released()), (2, 2));
}

/// The cheap checks refuse before the port is asked: `parameter_conflict` (overwrite without a path), a path
/// with no file name (`export_path`), an existing file without overwrite (`overwrite_required`, not `overloaded`).
#[test]
fn the_cheap_checks_refuse_before_the_port_is_asked() {
    let scratch = Scratch::new("cheap");
    let port = Port::saturated();
    let mut v = session(&port);
    let (code, _) = capture(&mut v, json!({"overwrite": true})).expect_err("refused");
    assert_eq!(code, "parameter_conflict");
    let (code, _) = capture(&mut v, json!({"path": ""})).expect_err("refused");
    assert_eq!(code, "export_path");
    let existing = scratch.path("existing.png");
    std::fs::write(&existing, b"kept").unwrap();
    let (code, _) = capture(&mut v, json!({"path": existing.to_str().unwrap()})).expect_err("refused");
    assert_eq!(code, "overwrite_required");
    assert_eq!(std::fs::read(&existing).unwrap(), b"kept", "the existing file is untouched");
    assert_eq!(port.asks(), 0, "no cheap refusal asks the port");
    // An overwrite passes the cheap checks and is refused by the port, the file still untouched.
    let (code, _) = capture(&mut v, json!({"path": existing.to_str().unwrap(), "overwrite": true})).expect_err("refused");
    assert_eq!(code, "overloaded");
    assert_eq!(std::fs::read(&existing).unwrap(), b"kept");
    assert_eq!(port.asks(), 1);
}

/// Without a path no file is written and no job is admitted: the reply is the frame's size and channel means.
#[test]
fn a_capture_without_a_path_writes_nothing_and_asks_nothing() {
    let port = Port::saturated();
    let mut v = session(&port);
    let reply = capture(&mut v, json!({})).expect("captured to the reply");
    assert!(reply["screen"]["width"].as_u64().is_some_and(|w| w > 0), "{reply}");
    assert!(reply.get("job").is_none(), "{reply}");
    assert_eq!(port.asks(), 0);
}

/// The derived resource is one per prototype file in every viewer; with a document open the capture names the open
/// document instead.
#[test]
fn the_job_resource_is_the_prototype_files_or_the_open_documents() {
    let scratch = Scratch::new("resource");
    let first = Port::open();
    let mut v = session(&first);
    capture(&mut v, json!({"path": scratch.path("a.png").to_str().unwrap()})).expect("captured");
    let second = Port::open();
    let mut w = session(&second);
    capture(&mut w, json!({"path": scratch.path("b.png").to_str().unwrap()})).expect("captured");
    let derived = first.scopes()[0].0.clone();
    assert_eq!(second.scopes()[0].0, derived, "one resource per prototype file, in every viewer");

    let port = Port::open();
    let mut d = Viewer::new(Prefer::Cpu);
    d.set_job_port(port.clone());
    d.open(&fixture("fig-prototype-pages-start.png")).expect("the PNG opens as a document");
    let document = d.run().snapshot().expect("a document is open").document().document_id.clone();
    let pages = fixture("fig-prototype-pages.fig");
    d.run_command("prototype.start_session", &obj(json!({"path": pages.to_str().unwrap()}))).expect("the fixture opens");
    let reply = capture(&mut d, json!({"path": scratch.path("c.png").to_str().unwrap()})).expect("captured");
    assert_eq!(reply["job"]["outcome"], "completed", "{reply}");
    let scopes = port.scopes();
    assert_eq!(scopes.last().map(|s| s.0.as_str()), Some(document.as_str()), "the open document: {scopes:?}");
    assert_ne!(document, derived);
}
