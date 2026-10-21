//! Argument boundary for the direct HTTP acquisition command.
#![forbid(unsafe_code)]

use std::ffi::OsString;
use std::path::PathBuf;

pub const HELP: &str = "Ferric Forager\nUsage: fforager fetch URL --output PATH --state-dir PATH [--max-bytes N] [--timeout-ms N] [--cancel-file PATH] [--allow-loopback IP:PORT]\nOnly direct HTTP/HTTPS inputs are supported. Existing output files are never overwritten.\n";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FetchArguments {
    pub url: String,
    pub output: PathBuf,
    pub state_dir: PathBuf,
    pub max_bytes: u64,
    pub timeout_ms: u64,
    pub cancel_file: Option<PathBuf>,
    pub allowed_loopback: Option<std::net::SocketAddr>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    Help,
    Version,
    Fetch(FetchArguments),
}

/// Parses only the bounded direct-fetch command and rejects ambiguous options.
///
/// # Errors
/// Returns an argument diagnostic for unknown, repeated, missing, or invalid values.
pub fn parse(arguments: impl IntoIterator<Item = OsString>) -> Result<Command, String> {
    let mut args = arguments.into_iter();
    let command = args
        .next()
        .ok_or_else(|| "missing command; use --help".to_owned())?;
    if command == "--help" || command == "-h" {
        return if args.next().is_none() {
            Ok(Command::Help)
        } else {
            Err("--help accepts no arguments".to_owned())
        };
    }
    if command == "--version" {
        return if args.next().is_none() {
            Ok(Command::Version)
        } else {
            Err("--version accepts no arguments".to_owned())
        };
    }
    if command != "fetch" {
        return Err("unsupported command; use fetch or --help".to_owned());
    }
    let mut url = None;
    let mut output = None;
    let mut state_dir = None;
    let mut max_bytes = None;
    let mut timeout_ms = None;
    let mut cancel_file = None;
    let mut allowed_loopback = None;
    while let Some(argument) = args.next() {
        match argument.to_str() {
            Some("--output") | Some("-o") => set_path(&mut output, args.next(), "--output")?,
            Some("--state-dir") => set_path(&mut state_dir, args.next(), "--state-dir")?,
            Some("--cancel-file") => set_path(&mut cancel_file, args.next(), "--cancel-file")?,
            Some("--allow-loopback") => {
                if allowed_loopback.is_some() {
                    return Err("duplicate --allow-loopback".to_owned());
                }
                let address = args
                    .next()
                    .and_then(|value| value.into_string().ok())
                    .and_then(|value| value.parse::<std::net::SocketAddr>().ok())
                    .filter(|address| address.ip().is_loopback() && address.port() != 0)
                    .ok_or_else(|| {
                        "--allow-loopback requires an exact loopback IP:PORT".to_owned()
                    })?;
                allowed_loopback = Some(address);
            }
            Some("--max-bytes") => {
                set_bound(&mut max_bytes, args.next(), "--max-bytes", 1, 1 << 40)?
            }
            Some("--timeout-ms") => {
                set_bound(&mut timeout_ms, args.next(), "--timeout-ms", 1, 3_600_000)?
            }
            Some(value) if !value.starts_with('-') => {
                if url.is_some() {
                    return Err("fetch accepts exactly one URL".to_owned());
                }
                if value.len() > 8192
                    || value.chars().any(char::is_control)
                    || !(value.starts_with("http://") || value.starts_with("https://"))
                {
                    return Err("input must be one bounded direct HTTP/HTTPS URL".to_owned());
                }
                url = Some(value.to_owned());
            }
            _ => return Err("unknown option or non-UTF-8 URL; use --help".to_owned()),
        }
    }
    let output = output.ok_or_else(|| "--output is required".to_owned())?;
    let state_dir = state_dir.ok_or_else(|| "--state-dir is required".to_owned())?;
    if output == state_dir {
        return Err("output and state directory must differ".to_owned());
    }
    Ok(Command::Fetch(FetchArguments {
        url: url.ok_or_else(|| "fetch requires a URL".to_owned())?,
        output,
        state_dir,
        max_bytes: max_bytes.unwrap_or(1 << 30),
        timeout_ms: timeout_ms.unwrap_or(120_000),
        cancel_file,
        allowed_loopback,
    }))
}

fn set_path(slot: &mut Option<PathBuf>, value: Option<OsString>, name: &str) -> Result<(), String> {
    if slot.is_some() {
        return Err(format!("duplicate {name}"));
    }
    let value = value.ok_or_else(|| format!("{name} requires a path"))?;
    if value.is_empty() || value.to_str().is_some_and(|text| text.starts_with('-')) {
        return Err(format!("{name} requires a nonempty path"));
    }
    *slot = Some(PathBuf::from(value));
    Ok(())
}

fn set_bound(
    slot: &mut Option<u64>,
    value: Option<OsString>,
    name: &str,
    minimum: u64,
    maximum: u64,
) -> Result<(), String> {
    if slot.is_some() {
        return Err(format!("duplicate {name}"));
    }
    let number = value
        .and_then(|value| value.into_string().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| (minimum..=maximum).contains(value))
        .ok_or_else(|| format!("{name} must be an integer in {minimum}..={maximum}"))?;
    *slot = Some(number);
    Ok(())
}
