//! Independent, bounded worker-lifetime diagnostics. Never controls a media job.

#![forbid(unsafe_code)]

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};
use sysinfo::{Pid, ProcessesToUpdate, System};

const MAX_LOG_BYTES: u64 = 1024 * 1024;
const POST_EXIT_RETENTION: Duration = Duration::from_secs(30);

#[derive(Debug)]
struct Options {
    state_dir: PathBuf,
    worker_pid: u32,
    ready_file: PathBuf,
    stop_file: PathBuf,
}

fn parse() -> Result<Options, String> {
    let mut arguments = std::env::args_os().skip(1);
    let mut values = std::collections::BTreeMap::new();
    while let Some(flag) = arguments.next() {
        let flag = flag
            .into_string()
            .map_err(|_| "invalid argument encoding")?;
        if !matches!(
            flag.as_str(),
            "--state-dir" | "--worker-pid" | "--ready-file" | "--stop-file"
        ) {
            return Err(format!("unknown watcher argument {flag}"));
        }
        let value = arguments.next().ok_or("missing watcher argument value")?;
        if values.insert(flag, value).is_some() {
            return Err("duplicate watcher argument".into());
        }
    }
    let mut required = |key: &str| values.remove(key).ok_or_else(|| format!("missing {key}"));
    let state_dir = required("--state-dir")?.into();
    let worker_pid = required("--worker-pid")?
        .to_str()
        .ok_or("invalid PID")?
        .parse::<u32>()
        .map_err(|_| "invalid PID")?;
    if worker_pid == 0 {
        return Err("zero worker PID".into());
    }
    Ok(Options {
        state_dir,
        worker_pid,
        ready_file: required("--ready-file")?.into(),
        stop_file: required("--stop-file")?.into(),
    })
}

fn event(
    log: &mut File,
    sequence: &mut u64,
    started: Instant,
    kind: &str,
    pid: u32,
) -> io::Result<()> {
    let value = serde_json::json!({
        "schema": "ff.watcher-lifetime@1",
        "sequence": *sequence,
        "monotonic_ms": started.elapsed().as_millis(),
        "event": kind,
        "worker_pid": pid,
        "watcher_pid": std::process::id(),
        "diagnostic_only": true,
    });
    let mut bytes = serde_json::to_vec(&value).map_err(io::Error::other)?;
    bytes.push(b'\n');
    if log.metadata()?.len().saturating_add(bytes.len() as u64) > MAX_LOG_BYTES {
        return Err(io::Error::other(
            "watcher diagnostic storage budget exhausted",
        ));
    }
    log.write_all(&bytes)?;
    log.sync_all()?;
    *sequence = sequence
        .checked_add(1)
        .ok_or_else(|| io::Error::other("sequence exhausted"))?;
    Ok(())
}

fn stop_requested(path: &Path) -> io::Result<bool> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    let mut bytes = Vec::new();
    Read::take(&mut file, 6).read_to_end(&mut bytes)?;
    Ok(bytes == b"stop\n")
}

fn heartbeat(root: &Path, worker_pid: u32, sequence: u64) -> io::Result<()> {
    let temporary = root.join("heartbeat.tmp");
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    let message = serde_json::json!({"schema":"ff.watcher-heartbeat@1", "worker_pid":worker_pid, "watcher_pid":std::process::id(), "sequence":sequence});
    serde_json::to_writer(&mut file, &message).map_err(io::Error::other)?;
    file.flush()?;
    drop(file);
    fs::rename(temporary, root.join("heartbeat.json"))
}

fn run(options: Options) -> io::Result<()> {
    let started = Instant::now();
    fs::create_dir_all(&options.state_dir)?;
    let log_path = options.state_dir.join(format!(
        "watcher-{}-{}.jsonl",
        options.worker_pid,
        std::process::id()
    ));
    let mut log = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(log_path)?;
    let mut sequence = fforager_diagnostics_contract::FIRST_SEQUENCE;
    let pid = Pid::from_u32(options.worker_pid);
    let mut system = System::new();
    system.refresh_processes(ProcessesToUpdate::Some(&[pid]), true);
    let worker_start = system.process(pid).map(sysinfo::Process::start_time);
    event(
        &mut log,
        &mut sequence,
        started,
        "watcher_started",
        options.worker_pid,
    )?;
    let ready = serde_json::json!({ "schema": "ff.watcher-ready@1", "worker_pid": options.worker_pid, "watcher_pid": std::process::id(), "ready": true });
    let mut ready_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&options.ready_file)?;
    serde_json::to_writer(&mut ready_file, &ready).map_err(io::Error::other)?;
    ready_file.sync_all()?;
    drop(ready_file);
    let mut exited_at = None;
    let mut heartbeat_sequence = 1_u64;
    loop {
        heartbeat(&options.state_dir, options.worker_pid, heartbeat_sequence)?;
        heartbeat_sequence = heartbeat_sequence
            .checked_add(1)
            .ok_or_else(|| io::Error::other("heartbeat sequence exhausted"))?;
        if stop_requested(&options.stop_file)? {
            event(
                &mut log,
                &mut sequence,
                started,
                "governed_shutdown",
                options.worker_pid,
            )?;
            return Ok(());
        }
        system.refresh_processes(ProcessesToUpdate::Some(&[pid]), true);
        let current = system.process(pid).map(sysinfo::Process::start_time);
        if worker_start.is_none() || current != worker_start {
            if exited_at.is_none() {
                event(
                    &mut log,
                    &mut sequence,
                    started,
                    "worker_exit_without_shutdown_receipt",
                    options.worker_pid,
                )?;
                exited_at = Some(Instant::now());
            }
            if exited_at.is_some_and(|when| when.elapsed() >= POST_EXIT_RETENTION) {
                event(
                    &mut log,
                    &mut sequence,
                    started,
                    "post_exit_observation_complete",
                    options.worker_pid,
                )?;
                return Ok(());
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn main() -> ExitCode {
    match parse().and_then(|options| run(options).map_err(|error| error.to_string())) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("diagnostics_degraded: {error}");
            ExitCode::from(1)
        }
    }
}
