//! Lane SMB-R60-FOLLOWUPS-1 items 1, 2 and 4 (feature `viewer`; validation round R60 attempt 2 probes j1,
//! j2 and g2): the document export and `export.run` are admitted through the viewer's bound JobPort
//! before the first byte is written (HBR-HOST-004, STU-ARC-008), with typed refusals and one settlement
//! per job; `page.add` returns when the document names the last page number.
//!
//! Oracles: port doubles that count every admit and release; the scratch folder's listing; a watchdog
//! thread for the mint.
#![cfg(feature = "viewer")]

mod support_context;

use hsk_studio_harness_core::document_new::{DocumentProfile, NewDocument};
use hsk_studio_harness_view::view::Viewer;
use hsk_studio_harness_view::view::doc_export::{DocExportRecipe, DocFormat};
use hsk_studio_harness_view::view::layout_export::LayoutPdfSettings;
use hsk_studio_harness_view::view::render::Prefer;
use hsk_studio_session::{Cost, JobPort, JobRefused, JobScope, JobTicket};
use serde_json::{Map, Value, json};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Duration;

/// Port double: refuses every admit when `refuse`, else admits; counts asks and releases.
#[derive(Default)]
struct Port {
    refuse: bool,
    asks: AtomicU64,
    released: AtomicU64,
}
impl Port {
    fn saturated() -> Arc<Self> {
        Arc::new(Self { refuse: true, ..Self::default() })
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
}
impl JobPort for Port {
    fn admit(&self, _scope: &JobScope<'_>, _cost: Cost) -> Result<JobTicket, JobRefused> {
        let n = self.asks.fetch_add(1, Ordering::SeqCst) + 1;
        if self.refuse { Err(JobRefused { retry_after_ms: 25 }) } else { Ok(JobTicket(n)) }
    }
    fn release(&self, _ticket: JobTicket) {
        self.released.fetch_add(1, Ordering::SeqCst);
    }
}

/// Test scratch directory under TMP, removed on drop.
struct Scratch(PathBuf);
impl Scratch {
    fn new(label: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("hsk-r60-followups-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
    fn listing(&self) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(&self.0).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        v.sort();
        v
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn vector_doc() -> Viewer {
    let mut v = support_context::viewer(Prefer::Cpu);
    v.new_document(&NewDocument::new(DocumentProfile::Vector, Some((128.0, 128.0))), None).unwrap();
    v
}

fn command(v: &mut Viewer, id: &str, params: Value) -> Result<Value, (String, Value)> {
    let map: Map<String, Value> = params.as_object().unwrap().clone();
    v.run_command(id, &map).map_err(|r| (r.code, r.detail))
}

fn png() -> DocExportRecipe {
    DocExportRecipe::of_format(DocFormat::Png { depth: 8 })
}

/// Item 1 (probe j1): a saturated port refuses the document export typed `overloaded`, asked exactly once,
/// with no file written; the layout PDF export through the same binding is the control.
#[test]
fn r60_j1_export_document_is_admitted_through_the_bound_job_port() {
    let scratch = Scratch::new("j1");
    let mut v = vector_doc();
    let port = Port::saturated();
    v.set_job_port(port.clone());
    let out = scratch.0.join("doc.png");
    let refused = v.export_document(&out, &png(), false).expect_err("a saturated port refuses the export");
    assert_eq!(refused.code, "overloaded", "{refused:?}");
    assert_eq!(port.asks(), 1, "the port is asked exactly once");
    assert!(scratch.listing().is_empty(), "no file written: {:?}", scratch.listing());
    assert_eq!(v.job_counts().refused_overloaded, 1);

    // Control: the layout PDF export through the same binding is refused the same way.
    v.layout_open(Some((200.0, 200.0)), Some(1)).unwrap();
    let pdf = scratch.0.join("layout.pdf");
    assert_eq!(v.export_layout_pdf(&pdf, false, &LayoutPdfSettings::default()).unwrap_err().code, "overloaded");
    assert_eq!(port.asks(), 2);
    assert!(scratch.listing().is_empty());
}

/// Item 1: an admitted export settles once: completed with the file, failed on a refusal after admission
/// (the destination exists), each releasing its ticket; the report carries the settlement.
#[test]
fn r60_j1_an_admitted_document_export_settles_completed_or_failed_once() {
    let scratch = Scratch::new("j1-settle");
    let mut v = vector_doc();
    let port = Port::open();
    v.set_job_port(port.clone());
    let out = scratch.0.join("doc.png");
    let report = v.export_document(&out, &png(), false).unwrap();
    assert_eq!(report["job"]["outcome"], "completed", "{report}");
    assert!(out.is_file());
    assert_eq!((port.asks(), port.released()), (1, 1));
    let failed = v.export_document(&out, &png(), false).unwrap_err();
    assert_eq!(failed.code, "destination_exists", "{failed:?}");
    assert_eq!((port.asks(), port.released()), (2, 2), "a failed export releases its ticket");
    let counts = v.job_counts();
    assert_eq!((counts.completed, counts.failed, counts.in_flight), (1, 1, 0), "{counts:?}");
}

/// Item 2 (probe j2): `export.run` asks the saturated port once before the first file and writes nothing;
/// with an open port it settles completed, one ticket for the run.
#[test]
fn r60_j2_export_run_is_admitted_through_the_bound_job_port() {
    let scratch = Scratch::new("j2");
    let mut v = vector_doc();
    let port = Port::saturated();
    v.set_job_port(port.clone());
    let params = json!({"dir": scratch.0.to_string_lossy(), "settings": [{"format": "PNG"}, {"format": "SVG"}]});
    let (code, detail) = command(&mut v, "export.run", params.clone()).expect_err("a saturated port refuses the run");
    assert_eq!(code, "overloaded", "{detail}");
    assert_eq!(port.asks(), 1, "the port is asked exactly once");
    assert!(scratch.listing().is_empty(), "no file written: {:?}", scratch.listing());

    let open = Port::open();
    v.set_job_port(open.clone());
    let report = command(&mut v, "export.run", params).unwrap();
    assert_eq!(report["count"], 2, "{report}");
    assert_eq!(report["job"]["outcome"], "completed", "{report}");
    assert_eq!((open.asks(), open.released()), (1, 1), "one admission for the whole run");
    // A refusal after admission (every file exists, no overwrite) settles failed and releases.
    let params = json!({"dir": scratch.0.to_string_lossy(), "settings": [{"format": "PNG"}]});
    assert_eq!(command(&mut v, "export.run", params).unwrap_err().0, "destination_exists");
    assert_eq!(open.asks(), 1, "a plan refusal is typed before the port is asked");
}

/// Item 4 (probe g2): a story naming `page-18446744073709551615` makes the page mint start at the last u64;
/// every `page.add` returns (a page or a typed refusal), never loops.
#[test]
fn r60_g2_page_add_returns_after_a_story_names_the_last_page_number() {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut v = vector_doc();
        v.place_text(10.0, 10.0, "page-18446744073709551615").unwrap();
        let first = v.add_page(None, false, Some((100.0, 100.0)), None);
        tx.send(("first", first.clone())).unwrap();
        let second = v.add_page(None, false, Some((100.0, 100.0)), None);
        tx.send(("second", second)).unwrap();
    });
    let mut got = Vec::new();
    for _ in 0..2 {
        match rx.recv_timeout(Duration::from_secs(20)) {
            Ok(r) => got.push(r),
            Err(_) => panic!("page.add did not return within 20 s (the mint loops); returned so far: {got:?}"),
        }
    }
    let (_, second) = &got[1];
    if let Err(e) = second {
        assert!(e.contains("page_number_exhausted"), "a typed refusal: {e}");
    }
}

/// Item 4 control: two page adds without the hostile story return page-1 and page-2.
#[test]
fn r60_g2_two_page_adds_without_a_hostile_story_mint_in_order() {
    let mut v = vector_doc();
    assert_eq!(v.add_page(None, false, Some((100.0, 100.0)), None).unwrap(), "page-1");
    assert_eq!(v.add_page(None, false, Some((100.0, 100.0)), None).unwrap(), "page-2");
}
