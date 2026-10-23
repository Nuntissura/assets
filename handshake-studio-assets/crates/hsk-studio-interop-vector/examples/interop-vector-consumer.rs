//! Interop-local real consumer: import an SVG into a proposal (summary printed as text, or
//! re-exported with `--export`), or identify an Illustrator file with `--ai`. A refusal prints a
//! stable error code on stderr and exits 2. Nothing is committed or written.
use hsk_studio_accord::CancellationToken;
use hsk_studio_interop_vector::*;
use std::{env, fs, process::ExitCode};

fn arg<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    let i = args.iter().position(|a| a == name)?;
    args.get(i + 1).map(String::as_str)
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    if args.iter().any(|a| a == "--descriptor") {
        println!("{DESCRIPTOR}");
        return ExitCode::SUCCESS;
    }
    let cancel = CancellationToken::default();
    if let Some(path) = arg(&args, "--ai") {
        let Ok(bytes) = fs::read(path) else {
            eprintln!("unreadable_input");
            return ExitCode::from(2);
        };
        println!("{:?}", identify_ai(&bytes));
        return match import_ai(&bytes, &ImportPolicy::default(), &cancel) {
            Ok(_) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("{}", e.code());
                ExitCode::from(2)
            }
        };
    }
    let Some(path) = arg(&args, "--svg") else {
        eprintln!("usage: interop-vector-consumer [--descriptor] [--svg FILE [--export]] [--ai FILE]");
        return ExitCode::from(2);
    };
    let Ok(bytes) = fs::read(path) else {
        eprintln!("unreadable_input");
        return ExitCode::from(2);
    };
    let proposal = match import_svg(&bytes, &ImportPolicy::default(), &mut SequentialIdMint::new(1), &cancel) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{}", e.code());
            return ExitCode::from(2);
        }
    };
    if args.iter().any(|a| a == "--export") {
        // The hierarchical export also writes the vector records (patterns, clips, masks); the
        // flat `export_svg` would draw their hidden source geometry.
        match export_proposal(&proposal, &ExportOptions::default()) {
            Ok(text) => print!("{text}"),
            Err(e) => {
                eprintln!("{}", e.code());
                return ExitCode::from(2);
            }
        }
        return ExitCode::SUCCESS;
    }
    println!(
        "paths={} layers={} styles={} viewport_px={:?}",
        proposal.paths.len(),
        proposal.snapshot.document().layers.len(),
        proposal.design.paint_styles.len(),
        proposal.provenance.viewport_px
    );
    for l in &proposal.loss {
        println!("loss {} {} {}", l.kind.wire(), l.code, l.path);
    }
    ExitCode::SUCCESS
}
