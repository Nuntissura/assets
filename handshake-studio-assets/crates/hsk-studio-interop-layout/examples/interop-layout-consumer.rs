//! Interop-local real consumer: import an IDML package and print the proposal summary and the loss
//! list, identify an InDesign binary file with `--indd`, or print the descriptor. A refusal prints a
//! stable error code on stderr and exits 2. Nothing is committed or written.
use hsk_studio_accord::CancellationToken;
use hsk_studio_interop_layout::*;
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
    let (cancel, limits) = (CancellationToken::default(), Limits::default());
    if let Some(path) = arg(&args, "--indd") {
        let Ok(bytes) = fs::read(path) else {
            eprintln!("unreadable_input");
            return ExitCode::from(2);
        };
        println!("{:?}", identify_indd(&bytes));
        return match import_indd(&bytes, &limits, &cancel) {
            Ok(never) => match never {},
            Err(e) => {
                eprintln!("{}", e.code());
                ExitCode::from(2)
            }
        };
    }
    let Some(path) = arg(&args, "--idml") else {
        eprintln!(
            "usage: interop-layout-consumer [--descriptor] [--idml FILE [--export]] [--indd FILE]"
        );
        return ExitCode::from(2);
    };
    let Ok(bytes) = fs::read(path) else {
        eprintln!("unreadable_input");
        return ExitCode::from(2);
    };
    let import = match import_idml(&bytes, &limits, &mut SequentialIdMint::new(1), &cancel) {
        Ok(i) => i,
        Err(e) => {
            eprintln!("{}", e.code());
            return ExitCode::from(2);
        }
    };
    if args.iter().any(|a| a == "--export") {
        match export_idml(&import, &cancel) {
            Ok(out) => println!("export_bytes={}", out.len()),
            Err(e) => {
                eprintln!("{}", e.code());
                return ExitCode::from(2);
            }
        }
        return ExitCode::SUCCESS;
    }
    let doc = import.proposal.snapshot.document();
    println!(
        "spreads={} pages={} frames={} threads={} stories={} parts={}",
        doc.page_spreads.len(),
        doc.page_spreads
            .iter()
            .map(|s| s.pages.len())
            .sum::<usize>(),
        doc.layers.len(),
        doc.text_threads().len(),
        import.proposal.stories.len(),
        import.package.parts.len()
    );
    for s in &import.proposal.stories {
        println!(
            "story reflow_certified={} composition={:?}",
            s.reflow_certified, s.composition
        );
    }
    for l in &import.loss {
        println!(
            "loss {} {} {} x{}",
            l.kind.wire(),
            l.code,
            l.element,
            l.count
        );
    }
    ExitCode::SUCCESS
}
