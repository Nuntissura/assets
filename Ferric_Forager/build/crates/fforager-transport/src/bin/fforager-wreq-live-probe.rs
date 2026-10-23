#![forbid(unsafe_code)]

use fforager_transport::{
    LiveProbeOptions, PersistedLiveProbeReport, WreqAdjudicationAdapter,
    validate_persisted_live_probe_report,
};
use serde::Serialize;
use std::fs::{self, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

const MAX_REPORT_BYTES: usize = 2 * 1024 * 1024;
static TEMPORARY_SEQUENCE: AtomicU64 = AtomicU64::new(1);

#[derive(Debug)]
struct RunOutcome {
    exit_code: i32,
    receipt: String,
}

#[derive(Debug)]
struct BoundedJsonWriter {
    bytes: Vec<u8>,
    observed: usize,
}

impl BoundedJsonWriter {
    fn new() -> Self {
        Self {
            bytes: Vec::new(),
            observed: 0,
        }
    }
}

impl io::Write for BoundedJsonWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.observed = self.observed.saturating_add(buffer.len());
        if self.observed > MAX_REPORT_BYTES {
            return Err(io::Error::other("bounded JSON report exceeded limit"));
        }
        self.bytes.extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn main() {
    let result = run(std::env::args().skip(1));
    match result {
        Ok(outcome) => {
            println!("{}", outcome.receipt);
            if outcome.exit_code != 0 {
                std::process::exit(outcome.exit_code);
            }
        }
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(2);
        }
    }
}

fn run(arguments: impl Iterator<Item = String>) -> Result<RunOutcome, String> {
    let output = parse_arguments(arguments)?;
    let options = LiveProbeOptions::explicitly_authorized(1024 * 1024);
    let result = WreqAdjudicationAdapter::new()
        .and_then(|adapter| adapter.execute_live_wire_observation(options));
    let (report, exit_code) = match result {
        Ok(evidence) => (PersistedLiveProbeReport::observed(evidence), 0),
        Err(error) => (PersistedLiveProbeReport::blocked(error.to_string()), 1),
    };
    write_persisted_report_atomic(&output, &report)?;
    let verdict = if exit_code == 0 {
        "observed_external_wire"
    } else {
        "blocked_external_wire"
    };
    Ok(RunOutcome {
        exit_code,
        receipt: format!("verdict={verdict}; report={}", output.display()),
    })
}

fn parse_arguments(arguments: impl Iterator<Item = String>) -> Result<PathBuf, String> {
    let arguments = arguments.collect::<Vec<_>>();
    let [enable, output_flag, output] = arguments.as_slice() else {
        return Err(
            "usage: fforager-wreq-live-probe --enable-live --output build/reports/NAME.json"
                .to_owned(),
        );
    };
    if enable != "--enable-live" || output_flag != "--output" {
        return Err(
            "usage: fforager-wreq-live-probe --enable-live --output build/reports/NAME.json"
                .to_owned(),
        );
    }
    report_path(output)
}

fn report_path(value: &str) -> Result<PathBuf, String> {
    let path = Path::new(value);
    let components = path.components().collect::<Vec<_>>();
    let valid = !value.contains(char::is_whitespace)
        && path.extension().and_then(|extension| extension.to_str()) == Some("json")
        && components
            .iter()
            .all(|component| matches!(component, Component::Normal(_)))
        && components
            .first()
            .is_some_and(|component| component.as_os_str() == "build")
        && components
            .get(1)
            .is_some_and(|component| component.as_os_str() == "reports")
        && components.len() >= 3;
    if valid {
        Ok(repository_root().join(path))
    } else {
        Err("output must be a space-free relative build/reports/NAME.json path".to_owned())
    }
}

fn write_persisted_report_atomic(
    path: &Path,
    report: &PersistedLiveProbeReport,
) -> Result<(), String> {
    let bytes = encode_bounded_json_report(report)?;
    validate_persisted_live_probe_report(&bytes).map_err(|error| error.to_string())?;
    validate_report_destination(path)?;
    write_report_bytes_atomic(path, &bytes)
}

#[cfg(test)]
fn write_json_report_atomic(path: &Path, report: &impl Serialize) -> Result<(), String> {
    let bytes = encode_bounded_json_report(report)?;
    write_report_bytes_atomic(path, &bytes)
}

fn encode_bounded_json_report(report: &impl Serialize) -> Result<Vec<u8>, String> {
    let mut writer = BoundedJsonWriter::new();
    if let Err(error) = serde_json::to_writer_pretty(&mut writer, report) {
        if writer.observed > MAX_REPORT_BYTES {
            return Err(format!(
                "FF-WREQ-E-REPORT-BOUNDS: {} exceeds {MAX_REPORT_BYTES}",
                writer.observed
            ));
        }
        return Err(format!("FF-WREQ-E-REPORT-ENCODE: {error}"));
    }
    if let Err(error) = writer.write_all(b"\n") {
        if writer.observed > MAX_REPORT_BYTES {
            return Err(format!(
                "FF-WREQ-E-REPORT-BOUNDS: {} exceeds {MAX_REPORT_BYTES}",
                writer.observed
            ));
        }
        return Err(format!("FF-WREQ-E-REPORT-ENCODE: {error}"));
    }
    Ok(writer.bytes)
}

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("transport manifest must be below build/crates")
        .to_path_buf()
}

fn validate_report_destination(path: &Path) -> Result<(), String> {
    let lexical_root = repository_root();
    let lexical_reports = lexical_root.join("build").join("reports");
    let parent = path
        .parent()
        .ok_or_else(|| "FF-WREQ-E-REPORT-PARENT: report has no parent".to_owned())?;
    if !path.is_absolute() || !parent.starts_with(&lexical_reports) {
        return Err(
            "FF-WREQ-E-REPORT-CONTAINMENT: report is outside canonical build/reports".to_owned(),
        );
    }
    let root = fs::canonicalize(&lexical_root)
        .map_err(|error| format!("FF-WREQ-E-REPORT-ROOT: {error}"))?;
    fs::create_dir_all(&lexical_reports)
        .map_err(|error| format!("FF-WREQ-E-REPORT-DIR: {error}"))?;
    let canonical_reports = fs::canonicalize(&lexical_reports)
        .map_err(|error| format!("FF-WREQ-E-REPORT-CONTAINMENT: {error}"))?;
    if !canonical_reports.starts_with(&root) {
        return Err(
            "FF-WREQ-E-REPORT-CONTAINMENT: build/reports resolves outside the repository"
                .to_owned(),
        );
    }
    fs::create_dir_all(parent).map_err(|error| format!("FF-WREQ-E-REPORT-DIR: {error}"))?;
    let canonical_parent = fs::canonicalize(parent)
        .map_err(|error| format!("FF-WREQ-E-REPORT-CONTAINMENT: {error}"))?;
    if !canonical_parent.starts_with(&canonical_reports) {
        return Err(
            "FF-WREQ-E-REPORT-CONTAINMENT: report parent resolves outside build/reports".to_owned(),
        );
    }
    Ok(())
}

fn write_report_bytes_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| "FF-WREQ-E-REPORT-PARENT: report has no parent".to_owned())?;
    fs::create_dir_all(parent).map_err(|error| format!("FF-WREQ-E-REPORT-DIR: {error}"))?;
    if path.exists() {
        return Err(format!(
            "FF-WREQ-E-REPORT-COLLISION: {} already exists",
            path.display()
        ));
    }
    let sequence = TEMPORARY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let extension = format!("json.{}.{}.tmp", std::process::id(), sequence);
    let temporary = path.with_extension(extension);
    let result = (|| {
        let mut temporary_file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|error| format!("FF-WREQ-E-REPORT-WRITE: {error}"))?;
        temporary_file
            .write_all(bytes)
            .map_err(|error| format!("FF-WREQ-E-REPORT-WRITE: {error}"))?;
        temporary_file
            .sync_all()
            .map_err(|error| format!("FF-WREQ-E-REPORT-SYNC: {error}"))?;
        drop(temporary_file);
        fs::hard_link(&temporary, path).map_err(|error| {
            if path.exists() {
                format!(
                    "FF-WREQ-E-REPORT-COLLISION: {} already exists",
                    path.display()
                )
            } else {
                format!("FF-WREQ-E-REPORT-COMMIT: {error}")
            }
        })
    })();
    match result {
        Ok(()) => {
            cleanup_after_commit(path, &temporary, |candidate| fs::remove_file(candidate));
            Ok(())
        }
        Err(error) => {
            let _ = fs::remove_file(&temporary);
            Err(error)
        }
    }
}

fn cleanup_after_commit(
    committed: &Path,
    temporary: &Path,
    cleanup: impl FnOnce(&Path) -> std::io::Result<()>,
) {
    if let Err(error) = cleanup(temporary) {
        eprintln!(
            "FF-WREQ-W-REPORT-CLEANUP: report committed at {}; temporary name {} requires cleanup: {error}",
            committed.display(),
            temporary.display()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};
    use std::thread;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temporary_directory(case: &str) -> PathBuf {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        repository_root()
            .join(".fforager-artifacts")
            .join("test-runs")
            .join(format!(
                "fforager-wreq-live-probe-{case}-{}-{timestamp}",
                std::process::id()
            ))
    }

    #[test]
    fn arguments_require_explicit_opt_in_and_report_path() {
        let error = parse_arguments(Vec::<String>::new().into_iter()).expect_err("missing args");
        assert!(error.starts_with("usage:"));
        let error = parse_arguments(
            ["--enable-live", "--output", "build/reports/../escape.json"]
                .map(str::to_owned)
                .into_iter(),
        )
        .expect_err("traversal");
        assert!(error.contains("space-free relative"));
        let path = parse_arguments(
            ["--enable-live", "--output", "build/reports/live-proof.json"]
                .map(str::to_owned)
                .into_iter(),
        )
        .expect("valid path");
        assert_eq!(
            path,
            repository_root().join("build/reports/live-proof.json")
        );
    }

    #[test]
    fn persisted_report_destination_is_repository_contained() {
        let outside = repository_root()
            .join(".fforager-artifacts")
            .join("test-runs")
            .join("escaped-report.json");
        let error = validate_report_destination(&outside).expect_err("outside report");
        assert!(error.contains("FF-WREQ-E-REPORT-CONTAINMENT"));
    }

    #[test]
    fn report_commit_refuses_overwrite_and_preserves_original() {
        let directory = temporary_directory("collision");
        let path = directory.join("report.json");
        write_report_bytes_atomic(&path, b"first\n").expect("first report");
        let error = write_report_bytes_atomic(&path, b"second\n").expect_err("collision");
        assert!(error.contains("FF-WREQ-E-REPORT-COLLISION"));
        assert_eq!(fs::read(&path).expect("read report"), b"first\n");
        fs::remove_dir_all(directory).expect("remove test directory");
    }

    #[test]
    fn oversized_report_is_rejected_before_filesystem_commit() {
        let directory = temporary_directory("bounds");
        let path = directory.join("report.json");
        let oversized = "x".repeat(MAX_REPORT_BYTES);
        let error = write_json_report_atomic(&path, &oversized).expect_err("report bound");
        assert!(error.contains("FF-WREQ-E-REPORT-BOUNDS"));
        assert!(!path.exists());
        assert!(!directory.exists());
    }

    #[test]
    fn persisted_report_uses_the_same_production_bound_before_commit() {
        let directory = temporary_directory("persisted-bounds");
        let path = directory.join("report.json");
        let report = PersistedLiveProbeReport::blocked("x".repeat(MAX_REPORT_BYTES));
        let error =
            write_persisted_report_atomic(&path, &report).expect_err("persisted report bound");
        assert!(error.contains("FF-WREQ-E-REPORT-BOUNDS"));
        assert!(!path.exists());
        assert!(!directory.exists());
    }

    #[test]
    fn concurrent_report_commit_has_exactly_one_winner() {
        let directory = temporary_directory("concurrent");
        let path = Arc::new(directory.join("report.json"));
        let barrier = Arc::new(Barrier::new(3));
        let workers = [b"alpha\n".as_slice(), b"bravo\n".as_slice()]
            .into_iter()
            .map(|payload| {
                let path = Arc::clone(&path);
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    barrier.wait();
                    write_report_bytes_atomic(&path, payload)
                })
            })
            .collect::<Vec<_>>();
        barrier.wait();
        let results = workers
            .into_iter()
            .map(|worker| worker.join().expect("writer thread"))
            .collect::<Vec<_>>();
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(results.iter().filter(|result| result.is_err()).count(), 1);
        let committed = fs::read(path.as_ref()).expect("committed report");
        assert!(matches!(committed.as_slice(), b"alpha\n" | b"bravo\n"));
        assert!(
            fs::read_dir(&directory)
                .expect("report directory")
                .all(|entry| entry
                    .expect("entry")
                    .path()
                    .extension()
                    .is_none_or(|extension| extension != "tmp"))
        );
        fs::remove_dir_all(directory).expect("remove test directory");
    }

    #[test]
    fn cleanup_failure_cannot_downgrade_a_committed_report() {
        let committed = Path::new("build/reports/committed.json");
        let temporary = Path::new("build/reports/.committed.json.tmp");
        cleanup_after_commit(committed, temporary, |_| {
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "mutation-only cleanup failure",
            ))
        });
    }
}
