//! Real Accord-local external consumer: no Folio, native or host dependencies.
use hsk_studio_accord::{
    CancellationToken, DESCRIPTOR, MAX_INPUT_BYTES, Ticks, ValidationError, parse_and_validate,
};
use std::{
    env,
    fs::File,
    io::{Read, Write},
};

fn run(args: &[String]) -> Result<String, String> {
    if args == ["--descriptor"] || args == ["--help"] {
        return Ok(DESCRIPTOR.into());
    }
    let mut input = None;
    let mut revision = None;
    let mut add_ticks = None;
    let mut canceled = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--cancel" => {
                if canceled {
                    return Err("duplicate_argument".into());
                }
                canceled = true;
                i += 1;
            }
            "--input" | "--expected-revision" | "--add-ticks" => {
                let value = args.get(i + 1).ok_or("missing_argument")?;
                match args[i].as_str() {
                    "--input" => {
                        if input.replace(value.clone()).is_some() {
                            return Err("duplicate_argument".into());
                        }
                    }
                    "--expected-revision" => {
                        let value = value.parse::<u64>().map_err(|_| "invalid_revision")?;
                        if revision.replace(value).is_some() {
                            return Err("duplicate_argument".into());
                        }
                    }
                    _ => {
                        let value = value.parse::<u64>().map_err(|_| "invalid_ticks")?;
                        if add_ticks.replace(value).is_some() {
                            return Err("duplicate_argument".into());
                        }
                    }
                }
                i += 2;
            }
            _ => return Err("unknown_argument".into()),
        }
    }
    let path = input.ok_or("missing_input")?;
    let revision = revision.ok_or("missing_expected_revision")?;
    let token = CancellationToken::default();
    if canceled {
        token.cancel()
    }
    token.check().map_err(|e| e.code())?;
    let file = File::open(path).map_err(|_| "unreadable_input")?;
    let mut bytes = vec![];
    file.take((MAX_INPUT_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| "read_error")?;
    if bytes.len() > MAX_INPUT_BYTES {
        return Err(ValidationError::InputLimit.code().into());
    }
    let input = std::str::from_utf8(&bytes).map_err(|_| "invalid_utf8")?;
    let result = parse_and_validate(input, revision, &token).map_err(|e| e.code())?;
    let effective = result
        .time()
        .checked_add(Ticks::new(add_ticks.unwrap_or(0)))
        .map_err(|e| e.code())?;
    let receipt = result.normalization();
    Ok(format!(
        "{{\"status\":\"accepted\",\"proof_kind\":\"accord_local_transport_only\",\"envelope\":{},\"effective_time_ticks\":{},\"normalization\":{{\"source_ticks_per_frame\":{},\"canonical_ticks_per_frame\":{},\"changed\":{}}}}}",
        result.to_json(),
        effective.value(),
        receipt.source_ticks_per_frame,
        receipt.canonical_ticks_per_frame,
        receipt.changed
    ))
}
fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    let (line, exit) = match run(&args) {
        Ok(line) => (line, 0),
        Err(code) => (
            format!(
                "{{\"status\":\"rejected\",\"code\":\"{}\",\"recovery\":\"Read --descriptor and repair only the rejected input; refresh caller revision/context independently.\"}}",
                code
            ),
            2,
        ),
    };
    if writeln!(std::io::stdout().lock(), "{line}").is_err() {
        std::process::exit(3)
    }
    std::process::exit(exit)
}
