//! EPS stage 2 (lane SMB-DXF-EPS-IMPORT, feature `eps-worker`): the isolated `stet` worker turns EPS
//! into PDF; PostScript loops, memory bombs and file operators stay inside capped, killed and
//! reaped worker processes. The worker binary is the one Cargo built for this test target.
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::StudioDesignRecords;
use hsk_studio_interop_vector_core::{
    AiImport, EpsPdfConverter, EpsWorkerOptions, ExportOptions, ImportPolicy, PdfDrawing, PdfDrawingPage, PdfDrawingProvider, PdfDrawItem, PdfEl, PdfPaint, PdfStroke,
    SequentialIdMint, VectorError, VectorPath, WorkerEpsConverter, export_eps, import_eps_with,
};
use hsk_studio_nib::{Anchor, Mirroring, Point, Winding};
use std::path::PathBuf;
use std::time::Duration;

fn worker() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_interop-vector-eps-worker"))
}

fn converter(options: EpsWorkerOptions) -> WorkerEpsConverter {
    WorkerEpsConverter::new(EpsWorkerOptions { program: Some(worker()), ..options }).expect("worker program")
}

fn convert(c: &WorkerEpsConverter, ps: &str) -> Result<Vec<u8>, VectorError> {
    c.eps_to_pdf(ps.as_bytes(), &CancellationToken::default())
}

fn assert_reaped(c: &WorkerEpsConverter) {
    let s = c.stats();
    assert_eq!(s.workers_live, 0, "every worker is reaped: {s:?}");
    assert_eq!(s.spawned, s.reaped, "spawned == reaped: {s:?}");
}

const SQUARE: &str = "%!PS-Adobe-3.0 EPSF-3.0\n%%BoundingBox: 0 0 100 50\n%%EndComments\n1 0 0 setrgbcolor 10 10 80 30 rectfill\n0 0 1 setrgbcolor 2 setlinewidth 5 5 moveto 95 45 lineto stroke\n/Helvetica findfont 12 selectfont 20 20 moveto (EPS) show\nshowpage\n%%EOF\n";

/// Provider that records the PDF it was given and returns a fixed one-page drawing.
struct Recording(std::cell::RefCell<Vec<u8>>);
impl PdfDrawingProvider for Recording {
    fn drawing(&self, pdf: &[u8], _: &CancellationToken) -> hsk_studio_interop_vector_core::Result<PdfDrawing> {
        *self.0.borrow_mut() = pdf.to_vec();
        let path = vec![PdfEl::MoveTo([5.0, 5.0]), PdfEl::LineTo([95.0, 45.0])];
        let stroke = PdfStroke { width: 2.0, cap: 0, join: 0, miter_limit: 10.0, dash: Vec::new(), dash_offset: 0.0 };
        let item = PdfDrawItem::Stroke { path, stroke, paint: PdfPaint::Solid([0.0, 0.0, 1.0, 1.0]), blend: 0, glyph: None, source: None };
        Ok(PdfDrawing { pages: vec![PdfDrawingPage { media_box: [0.0, 0.0, 100.0, 50.0], crop_box: [0.0, 0.0, 100.0, 50.0], items: vec![item], notes: Vec::new() }], ..PdfDrawing::default() })
    }
}

/// An EPS with fills, a stroke and text converts in the worker to a one-page PDF the size of the
/// bounding box; `import_eps_with` hands that PDF to the drawing provider and builds the proposal
/// through the shared builder with the `eps_via_pdf_conversion` loss.
#[test]
fn eps_converts_to_pdf_in_the_isolated_worker() {
    let c = converter(EpsWorkerOptions::default());
    let pdf = convert(&c, SQUARE).expect("conversion");
    assert!(pdf.starts_with(b"%PDF-"));
    let text = String::from_utf8_lossy(&pdf);
    assert!(text.contains("/MediaBox [0 0 100 50]") || text.contains("/MediaBox[0 0 100 50]"), "page is the bounding box");
    let provider = Recording(std::cell::RefCell::new(Vec::new()));
    let import: AiImport = import_eps_with(SQUARE.as_bytes(), &ImportPolicy::default(), &c, &provider, &mut SequentialIdMint::new(5), &CancellationToken::default()).expect("eps import");
    assert!(provider.0.borrow().starts_with(b"%PDF-"), "the provider read the worker's PDF");
    assert_eq!(import.proposal.provenance.source_format, "eps");
    assert!(import.proposal.has_loss_code("eps_via_pdf_conversion"));
    assert_eq!(import.proposal.paths.len(), 1);
    assert_reaped(&c);
}

/// An infinite PostScript loop is killed at the deadline (and, with Job Object caps, at the CPU
/// cap); a recursion bomb and a memory bomb end as typed failures; every worker is reaped.
#[test]
fn postscript_loops_and_bombs_are_contained() {
    let c = converter(EpsWorkerOptions { deadline: Duration::from_secs(3), cpu_time: Duration::from_secs(2), memory_bytes: 256 * 1024 * 1024, ..EpsWorkerOptions::default() });
    let looping = "%!PS-Adobe-3.0 EPSF-3.0\n%%BoundingBox: 0 0 10 10\n%%EndComments\n{ 1 pop } loop\n";
    assert_eq!(convert(&c, looping).err(), Some(VectorError::EpsConversionFailed("deadline")));
    let recursion = "%!PS-Adobe-3.0 EPSF-3.0\n%%BoundingBox: 0 0 10 10\n%%EndComments\n/r { r } def r\n";
    let r = convert(&c, recursion).err();
    assert!(matches!(r, Some(VectorError::EpsConversionFailed("postscript_error" | "crashed" | "deadline" | "memory_cap"))), "recursion: {r:?}");
    let bomb = "%!PS-Adobe-3.0 EPSF-3.0\n%%BoundingBox: 0 0 10 10\n%%EndComments\n[ 0 1 100000 { pop 16000000 string } for ]\n";
    let b = convert(&c, bomb).err();
    if hsk_studio_worker_caps() {
        assert_eq!(b, Some(VectorError::EpsConversionFailed("memory_cap")), "memory bomb under the 256 MiB cap");
    } else {
        println!("NOT_PROVEN: no Job Object caps in this build; memory bomb ended as {b:?} (deadline only)");
    }
    assert_reaped(&c);
}

fn hsk_studio_worker_caps() -> bool {
    cfg!(windows)
}

/// File operators cannot reach the disk or the protocol pipes: reading a file the test wrote,
/// writing, deleting, renaming, enumerating, `run` and `%stdin`/`%stdout` all fail inside the
/// worker; the files are untouched and the protocol stays in sync.
#[test]
fn file_operators_are_denied_in_the_worker() {
    let dir = std::env::temp_dir().join(format!("hsk-eps-worker-files-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let secret = dir.join("secret.txt");
    std::fs::write(&secret, b"secret").expect("secret file");
    let target = dir.join("written.txt");
    let ps_path = |p: &PathBuf| p.to_string_lossy().replace('\\', "/");
    let c = converter(EpsWorkerOptions::default());
    let attempts = [
        format!("({}) (r) file 100 string readstring pop pop", ps_path(&secret)),
        format!("({}) (w) file (x) writestring", ps_path(&target)),
        format!("({}) deletefile", ps_path(&secret)),
        format!("({}) ({}) renamefile", ps_path(&secret), ps_path(&target)),
        format!("({}/*) {{ pop }} 256 string filenameforall", ps_path(&dir)),
        format!("({}) run", ps_path(&secret)),
        "(%stdin) (r) file read pop pop".to_owned(),
        "(%stdout) (w) file (junk) writestring".to_owned(),
    ];
    // Plain PostScript (no EPSF line): the interpreter reports the program's errors (for EPSF it
    // tolerates them and keeps the page), so each denied operator surfaces as postscript_error.
    let program = |op: &str| format!("%!PS-Adobe-3.0\n{op}\n0 0 10 10 rectfill showpage\n");
    assert!(convert(&c, &program("")).expect("control program").starts_with(b"%PDF-"));
    assert_eq!(convert(&c, &program("1 0 div")).err(), Some(VectorError::EpsConversionFailed("postscript_error")), "errors surface");
    for op in &attempts {
        assert_eq!(convert(&c, &program(op)).err(), Some(VectorError::EpsConversionFailed("postscript_error")), "{op}");
    }
    // `status` reports the disk file as absent: the program would fail (undefined `nosuchop`) if
    // it saw the file.
    let status = program(&format!("({}) status {{ nosuchop }} if", ps_path(&secret)));
    assert!(convert(&c, &status).expect("status program").starts_with(b"%PDF-"));
    assert_eq!(std::fs::read(&secret).expect("secret intact"), b"secret");
    assert!(!target.exists(), "nothing was written");
    let _ = std::fs::remove_dir_all(&dir);
    assert_reaped(&c);
}

/// Studio's own EPSF export runs in the interpreter (the exported program is valid PostScript);
/// a missing worker program is `eps_converter_unavailable`; an output cap below the PDF size is
/// `output_over_cap`; an oversized input is refused before any process starts.
#[test]
fn exported_epsf_converts_and_caps_are_typed() {
    let anchor = |x: f64, y: f64| Anchor { position: Point::pt(x, y).unwrap(), incoming: Point::pt(0.0, 0.0).unwrap(), outgoing: Point::pt(0.0, 0.0).unwrap(), handle_mirroring: Mirroring::None };
    let path = VectorPath { path_id: "SVPT-00000001-0000-7000-8000-000000000001".into(), layer_id: "SLYR-00000001-0000-7000-8000-000000000002".into(), closed: true, anchors: vec![anchor(10.0, 10.0), anchor(60.0, 10.0), anchor(60.0, 40.0)], winding: Winding::NonZero };
    let design = StudioDesignRecords::new("SDOC-00000001-0000-7000-8000-000000000003");
    let eps = export_eps(&[path], &design, &ExportOptions { artboard_pt: Some([80.0, 50.0]), ..ExportOptions::default() }).expect("export");
    let c = converter(EpsWorkerOptions::default());
    let pdf = c.eps_to_pdf(eps.text.as_bytes(), &CancellationToken::default()).expect("exported EPS converts");
    assert!(pdf.starts_with(b"%PDF-"));
    let missing = WorkerEpsConverter::new(EpsWorkerOptions { program: Some(std::env::temp_dir().join("no-such-eps-worker.exe")), ..EpsWorkerOptions::default() });
    assert_eq!(missing.err(), Some(VectorError::EpsConverterUnavailable));
    let tiny = converter(EpsWorkerOptions { max_output_bytes: 64, ..EpsWorkerOptions::default() });
    assert_eq!(convert(&tiny, SQUARE).err(), Some(VectorError::EpsConversionFailed("output_over_cap")));
    let small_input = converter(EpsWorkerOptions { max_input_bytes: 8, ..EpsWorkerOptions::default() });
    assert_eq!(convert(&small_input, SQUARE).err(), Some(VectorError::BudgetExceeded("eps_worker_input")));
    assert_eq!(small_input.stats().spawned, 0);
    assert_reaped(&c);
    assert_reaped(&tiny);
}
