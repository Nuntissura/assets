//! Headless Studio harness: `studio-harness help` lists every command. Thin forwarder to
//! `hsk_studio_harness::cli::run`; opens no window and spawns no process. Pass paths under the
//! artifact root (never C:).
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let output = hsk_studio_harness::cli::run(&args);
    print!("{}", output.stdout);
    std::process::exit(output.exit);
}
