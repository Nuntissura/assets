//! Headless Studio harness: `studio-harness help` lists every command. Thin forwarder to
//! `hsk_studio_harness::cli::run_process`; opens no window and spawns no process. Pass paths under the
//! artifact root (never C:).
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let exit = hsk_studio_harness::cli::run_process(&args, &mut std::io::stdin().lock(), &mut std::io::stdout().lock());
    std::process::exit(exit);
}
