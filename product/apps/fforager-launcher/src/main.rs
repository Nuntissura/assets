//! Launcher owns a worker and a failure-isolated diagnostic watcher as siblings.
#![forbid(unsafe_code)]

use fforager_cli::Command as CliCommand;
use serde::Deserialize;
use std::ffi::OsString;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WatcherReady {
    schema: String,
    worker_pid: u32,
    watcher_pid: u32,
    ready: bool,
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(error) => {
            eprintln!(
                "{}",
                serde_json::json!({"status":"error","code":"launcher_failed","detail":error})
            );
            ExitCode::from(6)
        }
    }
}

fn run() -> Result<ExitCode, String> {
    let mut arguments: Vec<OsString> = std::env::args_os().skip(1).collect();
    let fetch = match fforager_cli::parse(arguments.clone()) {
        Ok(CliCommand::Help) => {
            print!("{}", fforager_cli::HELP);
            return Ok(ExitCode::SUCCESS);
        }
        Ok(CliCommand::Version) => {
            println!("fforager 0.1.0");
            return Ok(ExitCode::SUCCESS);
        }
        Ok(CliCommand::Fetch(fetch)) => fetch,
        Err(error) => {
            eprintln!(
                "{}",
                serde_json::json!({"status":"error","code":"invalid_arguments","detail":error})
            );
            return Ok(ExitCode::from(2));
        }
    };
    let directory = std::env::current_exe()
        .map_err(|error| error.to_string())?
        .parent()
        .ok_or("launcher has no executable directory")?
        .to_path_buf();
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_nanos();
    let control = fetch
        .state_dir
        .join("diagnostics")
        .join(format!("launcher-{}-{nonce}", std::process::id()));
    let diagnostic_root = match fs::create_dir_all(&control) {
        Ok(()) => Some(control),
        Err(error) => {
            degraded("diagnostic_state_unavailable", &error.to_string());
            None
        }
    };
    let cancel_path = fetch
        .cancel_file
        .clone()
        .or_else(|| diagnostic_root.as_ref().map(|root| root.join("cancel")));
    if fetch.cancel_file.is_none() {
        if let Some(path) = &cancel_path {
            arguments.push("--cancel-file".into());
            arguments.push(path.as_os_str().to_owned());
        }
    }
    let mut worker_command = Command::new(directory.join(executable("fforager-worker")));
    worker_command
        .args(&arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    configure_child(&mut worker_command);
    let mut worker = worker_command
        .spawn()
        .map_err(|error| format!("worker start failed: {error}"))?;
    let deadline = Instant::now() + Duration::from_millis(fetch.timeout_ms.saturating_add(5_000));
    let mut watcher = diagnostic_root
        .as_ref()
        .and_then(|root| start_watcher(&directory, root, worker.id()));
    let status = loop {
        match worker.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(error) => {
                let _ = worker.kill();
                let _ = worker.wait();
                if let Some((child, stop)) = watcher.take() {
                    stop_watcher(child, &stop);
                }
                return Err(format!("worker observation failed: {error}"));
            }
        }
        if Instant::now() >= deadline {
            if let Some(path) = &cancel_path {
                let _ = fs::write(path, b"cancel\n");
            }
            let grace = Instant::now() + Duration::from_millis(250);
            while Instant::now() < grace {
                if let Ok(Some(_)) = worker.try_wait() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            if worker
                .try_wait()
                .map_err(|error| error.to_string())?
                .is_none()
            {
                let _ = worker.kill();
            }
            let _ = worker.wait();
            if let Some((child, stop)) = watcher.take() {
                stop_watcher(child, &stop);
            }
            return Err(
                "worker exceeded the acquisition deadline and cancellation grace".to_owned(),
            );
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    if status.success() || status.code() == Some(130) {
        if let Some((child, stop)) = watcher.take() {
            stop_watcher(child, &stop);
        }
    } else if watcher.is_some() {
        degraded(
            "worker_failed_watcher_retained",
            "watcher keeps bounded crash evidence independently",
        );
    }
    let code = status
        .code()
        .and_then(|code| u8::try_from(code).ok())
        .unwrap_or(1);
    Ok(ExitCode::from(code))
}

fn start_watcher(directory: &Path, control: &Path, worker_pid: u32) -> Option<(Child, PathBuf)> {
    let ready = control.join("ready.json");
    let stop = control.join("stop");
    let mut command = Command::new(directory.join(executable("fforager-watcher")));
    command
        .arg("--state-dir")
        .arg(control)
        .arg("--worker-pid")
        .arg(worker_pid.to_string())
        .arg("--ready-file")
        .arg(&ready)
        .arg("--stop-file")
        .arg(&stop)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    configure_child(&mut command);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            degraded("watcher_missing_or_unavailable", &error.to_string());
            return None;
        }
    };
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if let Ok(file) = fs::File::open(&ready) {
            let mut bytes = Vec::new();
            if file.take(4097).read_to_end(&mut bytes).is_ok() && bytes.len() <= 4096 {
                if let Ok(message) = serde_json::from_slice::<WatcherReady>(&bytes) {
                    if message.schema == "ff.watcher-ready@1"
                        && message.worker_pid == worker_pid
                        && message.watcher_pid == child.id()
                        && message.ready
                        && matches!(child.try_wait(), Ok(None))
                    {
                        return Some((child, stop));
                    }
                }
            }
            // Publication may be partially visible; acceptance stays strict until the deadline.
        }
        match child.try_wait() {
            Ok(Some(_)) => {
                degraded("watcher_startup_failed", "watcher exited before readiness");
                return None;
            }
            Err(error) => {
                degraded("watcher_observation_failed", &error.to_string());
                stop_watcher(child, &stop);
                return None;
            }
            Ok(None) => {}
        }
        if Instant::now() >= deadline {
            degraded(
                "watcher_ready_timeout",
                "watcher readiness exceeded two seconds",
            );
            stop_watcher(child, &stop);
            return None;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn stop_watcher(mut child: Child, stop: &Path) {
    let _ = fs::write(stop, b"stop\n");
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return,
            Err(error) => {
                degraded("watcher_shutdown_observation_failed", &error.to_string());
                break;
            }
            Ok(None) => {}
        }
        if Instant::now() >= deadline {
            degraded(
                "watcher_shutdown_timeout",
                "watcher failed to stop within two seconds",
            );
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn degraded(code: &str, detail: &str) {
    eprintln!(
        "{}",
        serde_json::json!({"status":"diagnostics_degraded","code":code,"detail":detail})
    );
}
fn executable(name: &str) -> OsString {
    let mut name = OsString::from(name);
    name.push(std::env::consts::EXE_SUFFIX);
    name
}
#[cfg(windows)]
fn quiet(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    command.creation_flags(0x0800_0000);
}
#[cfg(not(windows))]
fn quiet(_command: &mut Command) {}
fn configure_child(command: &mut Command) {
    command.env_clear();
    for name in ["SystemRoot", "WINDIR", "TEMP", "TMP"] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    if let Some(system_root) = std::env::var_os("SystemRoot").or_else(|| std::env::var_os("WINDIR"))
    {
        command.env("PATH", PathBuf::from(system_root).join("System32"));
    }
    quiet(command);
}
