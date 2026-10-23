#![forbid(unsafe_code)]

use fforager_transport::{CorpusManifest, run_corpus};
use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::Read;
use std::path::{Path, PathBuf};

const DEFAULT_MANIFEST: &str = "build/fixtures/transport-v1/manifest.json";
const DEFAULT_REPORT: &str = "build/reports/wp-ff-007-transport-report.json";
const MAX_MANIFEST_BYTES: u64 = 512 * 1024;

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let (manifest_path, report_path) = parse_args(env::args().skip(1))?;
    let bytes = read_bounded(&manifest_path)?;
    let manifest: CorpusManifest = serde_json::from_slice(&bytes)
        .map_err(|error| format!("FF-TRANSPORT-E-MANIFEST-SCHEMA: {error}"))?;
    let report = run_corpus(&manifest)?;
    validate_negative_fixtures()?;
    write_report(&report_path, &report)?;
    println!(
        "{}; mandatory={}/{}; blocked={}; report={}",
        report.verdict.as_str(),
        report.summary.mandatory_passed,
        report.summary.mandatory_total,
        report.summary.mandatory_blocked,
        report_path.display()
    );
    Ok(())
}

fn read_bounded(path: &Path) -> Result<Vec<u8>, String> {
    let file =
        File::open(path).map_err(|error| format!("FF-TRANSPORT-E-MANIFEST-READ: {error}"))?;
    let mut bytes = Vec::new();
    file.take(MAX_MANIFEST_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("FF-TRANSPORT-E-MANIFEST-READ: {error}"))?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_MANIFEST_BYTES {
        return Err(format!(
            "FF-TRANSPORT-E-MANIFEST-BOUNDS: {} exceeds {MAX_MANIFEST_BYTES}",
            bytes.len()
        ));
    }
    Ok(bytes)
}

fn validate_negative_fixtures() -> Result<(), String> {
    let unknown_path = Path::new("build/fixtures/transport-v1/negative/unknown-field.json");
    let unknown = read_bounded(unknown_path)?;
    let unknown_error = match serde_json::from_slice::<CorpusManifest>(&unknown) {
        Ok(_) => {
            return Err(
                "FF-TRANSPORT-E-NEGATIVE-UNKNOWN-FIELD: fixture unexpectedly passed".to_owned(),
            );
        }
        Err(error) => error.to_string(),
    };
    if !unknown_error.contains("unknown field `undeclared_field`") {
        return Err(format!(
            "FF-TRANSPORT-E-NEGATIVE-UNKNOWN-FIELD: {unknown_error}"
        ));
    }
    let duplicate_path = Path::new("build/fixtures/transport-v1/negative/duplicate-case-id.json");
    let duplicate = read_bounded(duplicate_path)?;
    let duplicate_manifest: CorpusManifest = serde_json::from_slice(&duplicate)
        .map_err(|error| format!("FF-TRANSPORT-E-NEGATIVE-SCHEMA: {error}"))?;
    let Err(duplicate_error) = run_corpus(&duplicate_manifest) else {
        return Err("FF-TRANSPORT-E-NEGATIVE-DUPLICATE-ID: fixture unexpectedly passed".to_owned());
    };
    if !duplicate_error.contains("FF-TRANSPORT-E-CASE-ID: duplicate-id") {
        return Err(format!(
            "FF-TRANSPORT-E-NEGATIVE-DUPLICATE-ID: {duplicate_error}"
        ));
    }
    Ok(())
}

fn parse_args(args: impl Iterator<Item = String>) -> Result<(PathBuf, PathBuf), String> {
    let mut manifest = PathBuf::from(DEFAULT_MANIFEST);
    let mut report = PathBuf::from(DEFAULT_REPORT);
    let mut args = args;
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--manifest" => {
                manifest =
                    PathBuf::from(args.next().ok_or(
                        "usage: fforager-transport-corpus [--manifest PATH] [--report PATH]",
                    )?);
            }
            "--report" => {
                report =
                    PathBuf::from(args.next().ok_or(
                        "usage: fforager-transport-corpus [--manifest PATH] [--report PATH]",
                    )?);
            }
            _ => {
                return Err(
                    "usage: fforager-transport-corpus [--manifest PATH] [--report PATH]".to_owned(),
                );
            }
        }
    }
    Ok((manifest, report))
}

fn write_report(path: &Path, report: &fforager_transport::CorpusReport) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("FF-TRANSPORT-E-REPORT-DIR: {error}"))?;
    }
    let bytes = serde_json::to_vec_pretty(report)
        .map_err(|error| format!("FF-TRANSPORT-E-REPORT-ENCODE: {error}"))?;
    let temporary = path.with_extension(format!("json.{}.tmp", std::process::id()));
    let mut temporary_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|error| format!("FF-TRANSPORT-E-REPORT-WRITE: {error}"))?;
    std::io::Write::write_all(&mut temporary_file, &bytes)
        .map_err(|error| format!("FF-TRANSPORT-E-REPORT-WRITE: {error}"))?;
    temporary_file
        .sync_all()
        .map_err(|error| format!("FF-TRANSPORT-E-REPORT-SYNC: {error}"))?;
    drop(temporary_file);
    fs::rename(&temporary, path)
        .map_err(|error| format!("FF-TRANSPORT-E-REPORT-COMMIT: {error}"))?;
    Ok(())
}
