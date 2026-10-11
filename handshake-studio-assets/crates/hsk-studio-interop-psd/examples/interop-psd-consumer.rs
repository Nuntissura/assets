use hsk_studio_accord::CancellationToken;
use hsk_studio_interop_psd::{DESCRIPTOR, Limits, read_psd};
use std::{env, fs};

fn run() -> Result<(), String> {
    let args: Vec<String> = env::args().skip(1).collect();
    if args == ["--descriptor"] {
        println!("{DESCRIPTOR}");
        return Ok(());
    }
    let [flag, path] = args.as_slice() else {
        return Err("usage: --descriptor | --in <file.psd>".into());
    };
    if flag != "--in" {
        return Err("unknown flag".into());
    }
    let bytes = fs::read(path).map_err(|_| "unreadable input".to_string())?;
    let limits = Limits::default();
    let doc = read_psd(&bytes, &limits, &CancellationToken::default()).map_err(|e| e.code().to_string())?;
    println!(
        "{{\"version\":{},\"width\":{},\"height\":{},\"depth\":{},\"layers\":{},\"loss\":{}}}",
        doc.header.version,
        doc.header.width,
        doc.header.height,
        doc.header.depth,
        doc.layers().len(),
        doc.loss_report(&limits).to_json()
    );
    Ok(())
}

fn main() {
    if let Err(message) = run() {
        eprintln!("{message}");
        std::process::exit(1);
    }
}
