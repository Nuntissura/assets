use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::*;
use std::{
    env,
    fs::File,
    io::{self, Read, Write},
};
fn run() -> std::result::Result<(), String> {
    let args: Vec<String> = env::args().skip(1).collect();
    if args == ["--descriptor"] {
        println!("{DESCRIPTOR}");
        return Ok(());
    }
    if args == ["--schema"] {
        println!(
            "{}",
            serde_json::to_string(&document_schema()).map_err(|_| "schema serialization")?
        );
        return Ok(());
    }
    let mut input = None;
    let mut expected = None;
    let mut rename = None;
    let mut name = None;
    let mut successor = None;
    let mut cancel = false;
    let mut report = false;
    let mut budget = Budget::default();
    let mut i = 0;
    while i < args.len() {
        let flag = &args[i];
        if flag == "--report" {
            report = true;
            i += 1;
            continue;
        }
        if flag == "--cancel" {
            cancel = true;
            i += 1;
            continue;
        }
        let value = args.get(i + 1).ok_or("missing flag value")?;
        match flag.as_str() {
            "--input" => input = Some(value.clone()),
            "--expected-revision" => {
                expected = Some(value.parse::<u64>().map_err(|_| "invalid revision")?)
            }
            "--rename" => rename = Some(value.clone()),
            "--name" => name = Some(value.clone()),
            "--successor" => {
                successor = Some(value.parse::<u64>().map_err(|_| "invalid successor")?)
            }
            "--max-bytes" => budget.input_bytes = value.parse().map_err(|_| "invalid budget")?,
            "--max-nodes" => budget.nodes = value.parse().map_err(|_| "invalid budget")?,
            "--max-edges" => budget.edges = value.parse().map_err(|_| "invalid budget")?,
            "--max-payload-bytes" => {
                budget.payload_bytes = value.parse().map_err(|_| "invalid budget")?
            }
            _ => return Err("unknown flag".into()),
        }
        i += 2;
    }
    let expected = expected.ok_or("expected revision required")?;
    let path = input.ok_or("input required")?;
    // Read cap precedes allocation; no caller input path is included in diagnostics.
    if budget.input_bytes > 64 * 1024 * 1024 {
        return Err("input cap exceeds consumer safety ceiling".into());
    }
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(|_| "input unavailable")?
        .take((budget.input_bytes as u64) + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "input read failed")?;
    let token = CancellationToken::default();
    if cancel {
        token.cancel();
    }
    let inspection = inspect_bytes(&bytes, budget, &token, &Unresolved)
        .map_err(|d| format!("{:?}:{}", d.code, d.target))?;
    let snapshot = match inspection {
        Inspection::Editable(s) => s,
        Inspection::ReadOnly(r) => {
            return Err(format!(
                "read_only:{:?}:retained_bytes={}",
                r.diagnostic().code,
                r.encoded_bytes().len()
            ));
        }
    };
    if snapshot.document().revision != expected {
        return Err("StaleRevision:revision".into());
    }
    let output = match (rename, name, successor) {
        (None, None, None) => snapshot,
        (Some(target), Some(name), Some(next)) => snapshot
            .rename(&target, name, expected, next, budget, &token, &Unresolved)
            .map_err(|d| format!("{:?}:{}", d.code, d.target))?,
        _ => return Err("rename requires name and successor".into()),
    };
    if report {
        let dispositions:Vec<_>=output.resolution_issues().iter().map(|i|serde_json::json!({"target":i.target,"disposition":format!("{:?}",i.disposition)})).collect();
        println!(
            "{}",
            serde_json::json!({"source_only":true,"document_id":output.document().document_id,"revision":output.document().revision,"encoded_bytes":output.encoded_bytes().len(),"resolution_issues":dispositions})
        );
        return Ok(());
    }
    let mut stdout = io::stdout().lock();
    stdout
        .write_all(output.encoded_bytes())
        .and_then(|_| stdout.write_all(b"\n"))
        .map_err(|_| "output unavailable")?;
    Ok(())
}
fn main() {
    if let Err(e) = run() {
        eprintln!("{e}");
        std::process::exit(2);
    }
}
