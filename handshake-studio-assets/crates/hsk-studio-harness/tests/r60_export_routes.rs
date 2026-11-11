//! Lane SMB-R60-FOLLOWUPS-1, review F3 of c43f2d90 (lead rulings 2026-11-11; R60 probe class
//! export-job-not-admitted-jobport): `Viewer::export`, `export_vector_svg`, `export_vector_pdf` and
//! `export_develop` admit through the viewer's bound JobPort before the first byte. A saturated port
//! refuses each typed `overloaded`, is asked exactly once, and no file is written.
//! Oracles: a port double counting every admit; the output folder's listing.
#![cfg(feature = "viewer")]

mod common;

use common::{Scratch, synthetic_dng};
use hsk_studio_harness::view::Viewer;
use hsk_studio_harness::view::develop::DevelopExport;
use hsk_studio_harness::view::export::ExportFormat;
use hsk_studio_harness::view::render::Prefer;
use hsk_studio_harness_core::document_new::{DocumentProfile, NewDocument};
use hsk_studio_session::{Cost, JobPort, JobRefused, JobScope, JobTicket};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Port double: refuses every admit; counts asks.
#[derive(Default)]
struct Saturated {
    asks: AtomicU64,
}
impl JobPort for Saturated {
    fn admit(&self, _scope: &JobScope<'_>, _cost: Cost) -> Result<JobTicket, JobRefused> {
        self.asks.fetch_add(1, Ordering::SeqCst);
        Err(JobRefused { retry_after_ms: 25 })
    }
    fn release(&self, _ticket: JobTicket) {}
}
impl Saturated {
    fn asks(&self) -> u64 {
        self.asks.load(Ordering::SeqCst)
    }
}

/// An empty output folder in `scratch`.
fn out_dir(scratch: &Scratch) -> PathBuf {
    let out = scratch.0.join("out");
    std::fs::create_dir_all(&out).unwrap();
    out
}

fn listing(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect()
}

/// A vector document on a saturated port.
fn vector() -> (Viewer, Arc<Saturated>) {
    let mut v = Viewer::new(Prefer::Cpu);
    v.new_document(&NewDocument::new(DocumentProfile::Vector, Some((128.0, 128.0))), None).unwrap();
    let port = Arc::new(Saturated::default());
    v.set_job_port(port.clone());
    (v, port)
}

#[test]
fn r60_f3_viewer_export_is_admitted_through_the_bound_job_port() {
    let scratch = Scratch::new("r60-f3-export");
    let out = out_dir(&scratch);
    let (mut v, port) = vector();
    let refused = v.export(&out.join("a.png"), ExportFormat::Png, false).expect_err("a saturated port refuses the export");
    assert!(refused.contains("overloaded"), "typed: {refused}");
    assert_eq!(port.asks(), 1, "asked exactly once");
    assert!(listing(&out).is_empty(), "no file: {:?}", listing(&out));
}

#[test]
fn r60_f3_vector_svg_export_is_admitted_through_the_bound_job_port() {
    let scratch = Scratch::new("r60-f3-svg");
    let out = out_dir(&scratch);
    let (mut v, port) = vector();
    let refused = v.export_vector_svg(&out.join("a.svg"), false).expect_err("a saturated port refuses the export");
    assert!(refused.contains("overloaded"), "typed: {refused}");
    assert_eq!(port.asks(), 1, "asked exactly once");
    assert!(listing(&out).is_empty(), "no file: {:?}", listing(&out));
}

#[test]
fn r60_f3_vector_pdf_export_is_admitted_through_the_bound_job_port() {
    let scratch = Scratch::new("r60-f3-pdf");
    let out = out_dir(&scratch);
    let (mut v, port) = vector();
    let refused = v.export_vector_pdf(&out.join("a.pdf"), false).expect_err("a saturated port refuses the export");
    assert!(refused.contains("overloaded"), "typed: {refused}");
    assert_eq!(port.asks(), 1, "asked exactly once");
    assert!(listing(&out).is_empty(), "no file: {:?}", listing(&out));
}

#[test]
fn r60_f3_develop_export_is_admitted_through_the_bound_job_port() {
    let scratch = Scratch::new("r60-f3-develop");
    let source = scratch.0.join("synthetic.dng");
    std::fs::write(&source, synthetic_dng(64, 48)).unwrap();
    let out = out_dir(&scratch);
    let mut v = Viewer::new(Prefer::Cpu);
    v.open(&source).unwrap_or_else(|e| panic!("open: {e}"));
    let port = Arc::new(Saturated::default());
    v.set_job_port(port.clone());
    let refused = v.export_develop(&out.join("a.jpg"), DevelopExport::Jpeg { quality: 90 }, false).expect_err("a saturated port refuses the export");
    assert_eq!(refused.code, "overloaded", "{refused:?}");
    assert_eq!(port.asks(), 1, "asked exactly once");
    assert!(listing(&out).is_empty(), "no file: {:?}", listing(&out));
}
