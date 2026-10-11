//! Interop-local real consumer: `--descriptor` prints the descriptor; `--inspect FILE.fig` opens the
//! container (bounded, opaque preservation) and prints identity, parts, chunks and loss. A refusal
//! prints a stable error code on stderr and exits 2. Nothing is written. Editable support is always
//! NOT_PROVEN.
use hsk_studio_accord::CancellationToken;
use hsk_studio_interop_fig::{DESCRIPTOR, Limits, hex, open_fig};
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
    let Some(path) = arg(&args, "--inspect") else {
        eprintln!("usage: interop-fig-consumer [--descriptor] [--inspect FILE.fig]");
        return ExitCode::from(2);
    };
    let Ok(bytes) = fs::read(path) else {
        eprintln!("unreadable_input");
        return ExitCode::from(2);
    };
    let container = match open_fig(&bytes, &Limits::default(), &CancellationToken::default()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{}", e.code());
            return ExitCode::from(2);
        }
    };
    println!(
        "packaging={:?} kind={} version={} source_sha256={} editable=NOT_PROVEN",
        container.packaging,
        container.kind.label(),
        container.version,
        hex(&container.source_sha256)
    );
    for p in &container.parts {
        println!("part bytes={} sha256={}", p.bytes.len(), hex(&p.sha256));
    }
    for c in &container.chunks {
        println!("chunk {} len={} sha256={}", c.index, c.len, hex(&c.sha256));
    }
    ExitCode::SUCCESS
}
