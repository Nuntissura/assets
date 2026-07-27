#![forbid(unsafe_code)]

use fforager_transport::{
    LiveProbeOptions, PersistedLiveProbeReport, WreqAdjudicationAdapter,
    validate_persisted_live_probe_report,
};
use serde::Serialize;
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

const MAX_REPORT_BYTES: usize = 2 * 1024 * 1024;
static TEMPORARY_SEQUENCE: AtomicU64 = AtomicU64::new(1);

#[derive(Debug)]
struct RunOutcome {
    exit_code: i32,
    receipt: String,
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
    let options = LiveProbeOptions {
        maximum_body_bytes: 1024 * 1024,
    };
    let result =
        WreqAdjudicationAdapter::new().and_then(|adapter| adapter.execute_live_wire_probe(options));
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
        Ok(path.to_path_buf())
    } else {
        Err("output must be a space-free relative build/reports/NAME.json path".to_owned())
    }
}

fn write_persisted_report_atomic(
    path: &Path,
    report: &PersistedLiveProbeReport,
) -> Result<(), String> {
    let mut bytes = serde_json::to_vec_pretty(report)
        .map_err(|error| format!("FF-WREQ-E-REPORT-ENCODE: {error}"))?;
    bytes.push(b'\n');
    validate_persisted_live_probe_report(&bytes).map_err(|error| error.to_string())?;
    write_report_bytes_atomic(path, &bytes)
}

fn write_json_report_atomic(path: &Path, report: &impl Serialize) -> Result<(), String> {
    let mut bytes = serde_json::to_vec_pretty(report)
        .map_err(|error| format!("FF-WREQ-E-REPORT-ENCODE: {error}"))?;
    bytes.push(b'\n');
    if bytes.len() > MAX_REPORT_BYTES {
        return Err(format!(
            "FF-WREQ-E-REPORT-BOUNDS: {} exceeds {MAX_REPORT_BYTES}",
            bytes.len()
        ));
    }
    write_report_bytes_atomic(path, &bytes)
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
        std::env::temp_dir().join(format!(
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
        assert_eq!(path, Path::new("build/reports/live-proof.json"));
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
