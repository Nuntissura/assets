//! `studio-vk`: the asset-local Vulkan host binary. Prints one receipt JSON line on success, `error <code>/<detail>` on
//! stderr otherwise, and exits with the code in `cli::USAGE`. Every run but `help` sends one terminal observe frame,
//! a refused command line included (F-R60-22), attributed to the caller `--actor`, `--session` and `--resource` name
//! (else the asset-local test host's fixed context), to the `--observe` file, else to stderr ([`LineSink`]).
use hsk_studio_accord::{CancellationToken, CorrelationSource, TransientCorrelation};
use hsk_studio_native_host::HostError;
use hsk_studio_native_host::cli::{self, Caller, Command};
use hsk_studio_native_host::diag::{HostRun, LineSink};
use hsk_studio_native_host::{offscreen, window};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = match cli::parse(&args) {
        Ok(Command::Help) => {
            println!("{}", cli::USAGE);
            return;
        }
        other => other,
    };
    let (caller, refused) = match cli::caller(&args) {
        Ok(caller) => (caller, None),
        Err(e) => (Caller::asset_local(), Some(e)),
    };
    let mut sink = LineSink::new(cli::observe(&args), caller.caller_default);
    let cancel = CancellationToken::default();
    // The host mints the run's correlation, as the harness CLI does (one run per process).
    let correlation = TransientCorrelation::new().next().value();
    let mut host = match HostRun::new(caller.actor, caller.resource, correlation, &cancel, &mut sink) {
        Ok(host) => host,
        Err(e) => {
            eprintln!("error observe/{}", e.code());
            std::process::exit(HostError::Io("observe").exit_code());
        }
    };
    let result = run(command.and_then(|c| refused.map_or(Ok(c), Err)), &mut host);
    if let Some(Err(e)) = host.terminal() {
        eprintln!("observe_undelivered {}", e.code());
    }
    match result {
        Ok(line) => println!("{line}"),
        Err(e) => {
            eprintln!("error {e}");
            std::process::exit(e.exit_code());
        }
    }
}

fn run(command: Result<Command, HostError>, host: &mut HostRun<'_>) -> Result<String, HostError> {
    match command {
        Ok(Command::Offscreen(a)) => offscreen::run(&a, host).map(|r| r.receipt.render()),
        Ok(Command::Window(a)) => window::run(&a, host).map(|r| r.receipt.render()),
        // `main` answers help before a run starts; this arm still sends its frame.
        Ok(Command::Help) => {
            let _ = host.finish(Ok(()));
            Ok(cli::USAGE.to_string())
        }
        Err(e) => {
            eprintln!("{}", cli::USAGE);
            // A refused command line is a run: its failure goes through the caller's sink (F-R60-22).
            let _ = host.finish(Err(e));
            Err(e)
        }
    }
}
