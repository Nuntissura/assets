//! Separately terminable production worker; all acquisition uses the library facade.
#![forbid(unsafe_code)]

use fforager::{FetchRequest, fetch};
use fforager_cli::Command;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

fn main() -> std::process::ExitCode {
    let command = match fforager_cli::parse(std::env::args_os().skip(1)) {
        Ok(command) => command,
        Err(error) => {
            eprintln!(
                "{}",
                serde_json::json!({"status":"error","code":"invalid_arguments","detail":error})
            );
            return std::process::ExitCode::from(2);
        }
    };
    let args = match command {
        Command::Help => {
            print!("{}", fforager_cli::HELP);
            return std::process::ExitCode::SUCCESS;
        }
        Command::Version => {
            println!("fforager 0.1.0");
            return std::process::ExitCode::SUCCESS;
        }
        Command::Fetch(args) => args,
    };
    let request = FetchRequest {
        url: args.url,
        output: args.output,
        state_dir: args.state_dir,
        max_bytes: args.max_bytes,
        timeout_ms: args.timeout_ms,
        cancel_file: args.cancel_file,
        allowed_loopback: args.allowed_loopback,
    };
    let cancelled = Arc::new(AtomicBool::new(false));
    let done = Arc::new(AtomicBool::new(false));
    let monitor = request.cancel_file.as_ref().map(|path| {
        let path = path.clone();
        let cancelled = Arc::clone(&cancelled);
        let done = Arc::clone(&done);
        std::thread::spawn(move || {
            while !done.load(Ordering::Acquire) {
                if path.exists() {
                    cancelled.store(true, Ordering::Release);
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        })
    });
    let result = fetch(&request, &cancelled);
    done.store(true, Ordering::Release);
    if let Some(monitor) = monitor {
        if monitor.join().is_err() {
            eprintln!(
                "{}",
                serde_json::json!({"status":"diagnostics_degraded","code":"cancellation_monitor_failed"})
            );
        }
    }
    match result {
        Ok(result) => {
            println!(
                "{}",
                serde_json::json!({"status":"success","job_id":result.job_id,"output":result.output,"bytes":result.bytes,"sha256":result.sha256,"archive_sequence":result.archive_sequence,"recovered":result.recovered})
            );
            std::process::ExitCode::SUCCESS
        }
        Err(error) => {
            let (code, exit) = match &error {
                fforager::FetchError::InvalidRequest(_) => ("invalid_request", 2),
                fforager::FetchError::Cancelled => ("cancelled", 130),
                fforager::FetchError::TimedOut => ("timed_out", 124),
                fforager::FetchError::Transport(_) => ("transport_failed", 3),
                fforager::FetchError::Storage(_) => ("storage_failed", 4),
                fforager::FetchError::LimitExceeded(_) => ("limit_exceeded", 5),
            };
            eprintln!(
                "{}",
                serde_json::json!({"status":"error","code":code,"detail":error.to_string()})
            );
            std::process::ExitCode::from(exit)
        }
    }
}
