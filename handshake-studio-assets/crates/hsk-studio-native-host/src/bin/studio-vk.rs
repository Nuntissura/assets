//! `studio-vk`: the asset-local Vulkan host binary. Prints one receipt JSON line on success, `error <code>/<detail>` on
//! stderr otherwise, and exits with the code in `cli::USAGE`.
use hsk_studio_native_host::cli::{self, Command};
use hsk_studio_native_host::{offscreen, window};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match cli::parse(&args) {
        Ok(Command::Help) => {
            println!("{}", cli::USAGE);
            return;
        }
        Ok(Command::Offscreen(a)) => offscreen::run(&a).map(|r| r.receipt.render()),
        Ok(Command::Window(a)) => window::run(&a).map(|r| r.receipt.render()),
        Err(e) => {
            eprintln!("{}", cli::USAGE);
            Err(e)
        }
    };
    match result {
        Ok(line) => println!("{line}"),
        Err(e) => {
            eprintln!("error {e}");
            std::process::exit(e.exit_code());
        }
    }
}
