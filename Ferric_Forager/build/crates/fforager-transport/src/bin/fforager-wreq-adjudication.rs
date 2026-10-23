#![forbid(unsafe_code)]

use fforager_transport::{run_wreq_adjudication, validate_wreq_adjudication_report};
use serde::Serialize;
use std::fs::{self, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

const MAX_REPORT_BYTES: usize = 1024 * 1024;
static TEMPORARY_SEQUENCE: AtomicU64 = AtomicU64::new(1);

#[derive(Debug)]
struct BoundedWriter {
    bytes: Vec<u8>,
    observed: usize,
}

impl io::Write for BoundedWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.observed = self.observed.saturating_add(buffer.len());
        if self.observed > MAX_REPORT_BYTES {
            return Err(io::Error::other("adjudication report exceeded limit"));
        }
        self.bytes.extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn main() {
    match run(std::env::args().skip(1)) {
        Ok(receipt) => println!("{receipt}"),
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(2);
        }
    }
}

fn run(arguments: impl Iterator<Item = String>) -> Result<String, String> {
    let output = parse_arguments(arguments)?;
    let report = run_wreq_adjudication().map_err(|error| error.to_string())?;
    let verdict = report.verdict().as_str();
    let bytes = encode_bounded(&report)?;
    validate_wreq_adjudication_report(&bytes).map_err(|error| error.to_string())?;
    write_atomic(&output, &bytes)?;
    Ok(format!("{verdict}; report={}", output.display()))
}

fn parse_arguments(arguments: impl Iterator<Item = String>) -> Result<PathBuf, String> {
    let arguments = arguments.collect::<Vec<_>>();
    let [output_flag, output] = arguments.as_slice() else {
        return Err(
            "usage: fforager-wreq-adjudication --output build/reports/NAME.json".to_owned(),
        );
    };
    if output_flag != "--output" {
        return Err(
            "usage: fforager-wreq-adjudication --output build/reports/NAME.json".to_owned(),
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

fn encode_bounded(report: &impl Serialize) -> Result<Vec<u8>, String> {
    let mut writer = BoundedWriter {
        bytes: Vec::new(),
        observed: 0,
    };
    if let Err(error) = serde_json::to_writer_pretty(&mut writer, report) {
        if writer.observed > MAX_REPORT_BYTES {
            return Err(format!(
                "FF-WREQ-E-REPORT-BOUNDS: {} exceeds {MAX_REPORT_BYTES}",
                writer.observed
            ));
        }
        return Err(format!("FF-WREQ-E-REPORT-ENCODE: {error}"));
    }
    writer
        .write_all(b"\n")
        .map_err(|error| format!("FF-WREQ-E-REPORT-ENCODE: {error}"))?;
    Ok(writer.bytes)
}

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("transport manifest must be below build/crates")
        .to_path_buf()
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let lexical_root = repository_root();
    let lexical_reports = lexical_root.join("build").join("reports");
    let parent = path
        .parent()
        .ok_or_else(|| "FF-WREQ-E-REPORT-PARENT: report has no parent".to_owned())?;
    if !path.is_absolute() || !parent.starts_with(&lexical_reports) {
        return Err("FF-WREQ-E-REPORT-CONTAINMENT: report is outside build/reports".to_owned());
    }
    let root = fs::canonicalize(&lexical_root)
        .map_err(|error| format!("FF-WREQ-E-REPORT-ROOT: {error}"))?;
    fs::create_dir_all(&lexical_reports)
        .map_err(|error| format!("FF-WREQ-E-REPORT-DIR: {error}"))?;
    let canonical_reports = fs::canonicalize(&lexical_reports)
        .map_err(|error| format!("FF-WREQ-E-REPORT-CONTAINMENT: {error}"))?;
    if !canonical_reports.starts_with(&root) {
        return Err(
            "FF-WREQ-E-REPORT-CONTAINMENT: build/reports resolves outside repository".to_owned(),
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
    if path.exists() {
        return Err(format!(
            "FF-WREQ-E-REPORT-COLLISION: {} already exists",
            path.display()
        ));
    }
    let sequence = TEMPORARY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temporary = path.with_extension(format!("json.{}.{}.tmp", std::process::id(), sequence));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|error| format!("FF-WREQ-E-REPORT-WRITE: {error}"))?;
        file.write_all(bytes)
            .map_err(|error| format!("FF-WREQ-E-REPORT-WRITE: {error}"))?;
        file.sync_all()
            .map_err(|error| format!("FF-WREQ-E-REPORT-SYNC: {error}"))?;
        drop(file);
        fs::hard_link(&temporary, path).map_err(|error| format!("FF-WREQ-E-REPORT-COMMIT: {error}"))
    })();
    match result {
        Ok(()) => {
            if let Err(error) = fs::remove_file(&temporary) {
                eprintln!(
                    "FF-WREQ-W-REPORT-CLEANUP: committed {}; temporary {} remains: {error}",
                    path.display(),
                    temporary.display()
                );
            }
            Ok(())
        }
        Err(error) => {
            let _ = fs::remove_file(&temporary);
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_requires_contained_report_path() {
        assert!(parse_arguments(Vec::<String>::new().into_iter()).is_err());
        assert!(
            parse_arguments(
                ["--output", "build/reports/../escape.json"]
                    .map(str::to_owned)
                    .into_iter()
            )
            .is_err()
        );
        assert_eq!(
            parse_arguments(
                ["--output", "build/reports/adjudication.json"]
                    .map(str::to_owned)
                    .into_iter()
            )
            .expect("valid path"),
            repository_root().join("build/reports/adjudication.json")
        );
    }

    #[test]
    fn aggregate_report_round_trips_through_the_strict_consumer() {
        let report = run_wreq_adjudication().expect("adjudication");
        let bytes = encode_bounded(&report).expect("bounded report");
        validate_wreq_adjudication_report(&bytes).expect("strict report");
    }
}
