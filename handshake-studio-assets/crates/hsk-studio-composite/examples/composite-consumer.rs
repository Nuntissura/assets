use hsk_studio_accord::CancellationToken;
use hsk_studio_composite::{
    BlendSpace, DESCRIPTOR, Limits, Step, StepStatus, blend::MODE_TABLE, evaluate, lower,
};
use hsk_studio_folio::{Budget, Inspection, Unresolved, inspect_bytes};
use std::{
    env,
    fs::File,
    io::Read,
};

const MAX_DOC_BYTES: u64 = 8 * 1024 * 1024;

fn run() -> Result<(), String> {
    let args: Vec<String> = env::args().skip(1).collect();
    if args == ["--descriptor"] {
        println!("{DESCRIPTOR}");
        return Ok(());
    }
    if args == ["--modes"] {
        for row in MODE_TABLE {
            println!(
                "{:>2} {:<18} {:<11} {}",
                row.mode.discriminant(),
                row.key,
                row.fidelity.key(),
                row.reference
            );
        }
        return Ok(());
    }
    let mut doc = None;
    let mut expected = None;
    let mut space = BlendSpace::Encoded;
    let mut cancel = false;
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--cancel" {
            cancel = true;
            i += 1;
            continue;
        }
        let value = args.get(i + 1).ok_or("missing flag value")?;
        match args[i].as_str() {
            "--doc" => doc = Some(value.clone()),
            "--expected-revision" => {
                expected = Some(value.parse::<u64>().map_err(|_| "invalid revision")?)
            }
            "--space" => space = BlendSpace::parse(value).ok_or("space must be encoded|linear")?,
            _ => return Err("unknown flag".into()),
        }
        i += 2;
    }
    let path = doc.ok_or("usage: composite-consumer --descriptor | --modes | --doc FILE --expected-revision N [--space encoded|linear] [--cancel]")?;
    let expected = expected.ok_or("expected revision required")?;
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(|_| "document unavailable")?
        .take(MAX_DOC_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "document read failed")?;
    if bytes.len() as u64 > MAX_DOC_BYTES {
        return Err("document exceeds consumer ceiling".into());
    }
    let token = CancellationToken::default();
    if cancel {
        token.cancel();
    }
    let snapshot = match inspect_bytes(&bytes, Budget::default(), &token, &Unresolved)
        .map_err(|d| format!("{:?}:{}", d.code, d.target))?
    {
        Inspection::Editable(s) => s,
        Inspection::ReadOnly(_) => return Err("document is read-only; not lowered".into()),
    };
    let plan = lower(&snapshot, expected, space, &Limits::default(), &token)
        .map_err(|e| e.to_string())?;
    let report = evaluate(&plan, &snapshot, &token).map_err(|e| e.to_string())?;
    println!(
        "revision {} space {} profile_bound {} lowest {}",
        plan.revision,
        plan.blend_space.key(),
        plan.blend_profile_bound,
        report.receipt.blend.lowest.key()
    );
    for (step, eval) in plan.steps.iter().zip(&report.steps) {
        let kind = match step {
            Step::Source { .. } => "source",
            Step::Composite { .. } => "composite",
        };
        let extent = eval
            .extent
            .map(|r| format!("{},{} {}x{}", r.x, r.y, r.width, r.height))
            .unwrap_or_else(|| "none".into());
        let status = match &eval.status {
            StepStatus::Ready => "ready".to_string(),
            StepStatus::Unavailable(r) => format!("unavailable({r})"),
        };
        println!(
            "{kind} {} visible={} extent={extent} {status}",
            eval.addr.text(),
            eval.visible
        );
        if let Step::Composite { inputs, .. } = step {
            for (n, input) in inputs.iter().enumerate() {
                println!("  input {n} {}", input.text());
            }
        }
    }
    for u in &report.unavailable {
        println!("unavailable {} {}", u.target, u.reason);
    }
    Ok(())
}

fn main() {
    if let Err(message) = run() {
        eprintln!("{message}");
        std::process::exit(2);
    }
}
