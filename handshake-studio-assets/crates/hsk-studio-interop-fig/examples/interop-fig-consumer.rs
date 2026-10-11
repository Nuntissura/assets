//! Interop-local real consumer: `--descriptor` prints the descriptor; `--inspect FILE.fig` opens the
//! container (bounded, opaque preservation) and prints identity, parts, chunks, the Kiwi schema
//! summary, the census and the loss list. Inspection admits the file's own (kind, version) so a
//! newly supplied real file can be examined; that is NOT a recorded observed version. A refusal
//! prints a stable error code on stderr and exits 2. Nothing is written. Editable support is reported
//! NOT_PROVEN.
use hsk_studio_accord::CancellationToken;
use hsk_studio_interop_fig::{
    DESCRIPTOR, FigOutcome, Limits, VersionPolicy, hex, import_fig, open_fig,
};
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
    let (limits, cancel) = (Limits::default(), CancellationToken::default());
    let container = match open_fig(&bytes, &limits, &cancel) {
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
    let policy = VersionPolicy::default().with_supported(container.kind, container.version);
    match import_fig(&bytes, &limits, &policy, None, &cancel) {
        Ok(FigOutcome::Imported(i)) => {
            if let Some(s) = &i.schema_summary {
                println!(
                    "schema chunk={} definitions={}",
                    s.chunk_index,
                    s.definitions.len()
                );
            }
            if let Some(c) = &i.census {
                println!("census nodes={} roots={}", c.node_count, c.root_ids.len());
                for (name, n) in &c.node_type_histogram {
                    println!("census type {name} {n}");
                }
                for id in &c.root_ids {
                    println!("census root {id}");
                }
            }
            for l in &i.loss {
                println!("loss {} {} {}", l.kind.wire(), l.code, l.part);
            }
            ExitCode::SUCCESS
        }
        Ok(FigOutcome::OpaqueOnly { error, .. }) => {
            eprintln!("{}", error.code());
            ExitCode::from(2)
        }
        Err(e) => {
            eprintln!("{}", e.code());
            ExitCode::from(2)
        }
    }
}
