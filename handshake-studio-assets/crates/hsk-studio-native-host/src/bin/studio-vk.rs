//! `studio-vk`: the asset-local Vulkan host binary. Prints one receipt JSON line on success, `error <code>/<detail>` on
//! stderr otherwise, and exits with the code in `cli::USAGE`. Every run but `help` sends one terminal observe frame,
//! a refused command line included (F-R60-22), attributed to the caller `--actor`, `--session` and `--resource` name
//! (else the asset-local test host's fixed context), to the `--observe` file, else to stderr ([`LineSink`]). Each run
//! has its own correlation. A failed write to stderr is ignored: it changes neither the exit code nor the receipt.
use hsk_studio_accord::{CancellationToken, CorrelationSource, TransientCorrelation};
use hsk_studio_native_host::HostError;
use hsk_studio_native_host::cli::{self, Caller, Command};
use hsk_studio_native_host::diag::{HostRun, LineSink};
use hsk_studio_native_host::{offscreen, window};
use std::ffi::OsString;
use std::io::Write as _;
use std::time::{SystemTime, UNIX_EPOCH};

/// One line to stderr; a failed write is ignored (review fc7f4215 F2).
fn note(line: std::fmt::Arguments<'_>) {
    let _ = writeln!(std::io::stderr().lock(), "{line}");
}

/// This run's correlation, minted by the host through accord's `TransientCorrelation` from the process birth: the low
/// 64 bits of the start time in nanoseconds mixed with the process id, so separate runs get separate correlations.
fn run_correlation() -> u64 {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos() as u64);
    let birth = nanos ^ u64::from(std::process::id()).rotate_left(32);
    TransientCorrelation::starting_at(birth.max(1)).next().value()
}

fn main() {
    let raw: Vec<OsString> = std::env::args_os().skip(1).collect();
    // A non-Unicode argument is a usage refusal that sends its frame like any other (review fc7f4215 F1).
    let args: Result<Vec<String>, HostError> =
        raw.iter().map(|a| a.to_str().map(str::to_owned).ok_or(HostError::Usage("argument_encoding"))).collect();
    let command = match &args {
        Ok(args) => match cli::parse(args) {
            Ok(Command::Help) => {
                println!("{}", cli::USAGE);
                return;
            }
            other => other,
        },
        Err(e) => Err(*e),
    };
    let (caller, refused) = match args.as_deref().map_err(|e| *e).and_then(cli::caller::<String>) {
        Ok(caller) => (caller, None),
        Err(e) => (Caller::asset_local(), Some(e)),
    };
    let mut sink = LineSink::new(cli::observe(&raw), caller.caller_default);
    let cancel = CancellationToken::default();
    let mut host = match HostRun::new(caller.actor, caller.resource, run_correlation(), &cancel, &mut sink) {
        Ok(host) => host,
        Err(e) => {
            note(format_args!("error observe/{}", e.code()));
            std::process::exit(HostError::Io("observe").exit_code());
        }
    };
    let result = run(command.and_then(|c| refused.map_or(Ok(c), Err)), &mut host);
    let undelivered = match host.terminal() {
        Some(Err(e)) => Some(e.code()),
        _ => None,
    };
    match result {
        Ok(line) => {
            // The receipt first: a sink that did not take the frame changes neither it nor the exit code.
            println!("{line}");
            if let Some(code) = undelivered {
                note(format_args!("observe_undelivered {code}"));
            }
        }
        Err(e) => {
            if let Some(code) = undelivered {
                note(format_args!("observe_undelivered {code}"));
            }
            note(format_args!("error {e}"));
            std::process::exit(e.exit_code());
        }
    }
}

fn run(command: Result<Command, HostError>, host: &mut HostRun<'_>) -> Result<String, HostError> {
    match command {
        Ok(Command::Offscreen(a)) => offscreen::run(&a, host).map(|r| r.receipt.render()),
        Ok(Command::Window(a)) => window::run(&a, host).map(|r| r.receipt.render()),
        // Not reached: `main` answers help, which is no run and sends no frame, before a HostRun exists.
        Ok(Command::Help) => Ok(cli::USAGE.to_string()),
        Err(e) => {
            note(format_args!("{}", cli::USAGE));
            // A refused command line is a run: its failure goes through the caller's sink (F-R60-22).
            let _ = host.finish(Err(e));
            Err(e)
        }
    }
}
