//! MotionJS-local consumer over the deterministic scripted test double (no JavaScript engine is
//! linked yet). `motion-js-consumer --eval "num:3" [--engine scripted] [--fuel N] [--dimension N]`
use hsk_studio_accord::CancellationToken;
use hsk_studio_motion::{EvalTick, ExprRequest, ExpressionProvider, Fuel, HostRead, NoHost};
use hsk_studio_motion_js::*;
use std::env;

fn run() -> Result<(), String> {
    let args: Vec<_> = env::args().skip(1).collect();
    if args == ["--descriptor"] || args == ["--help"] {
        println!("{DESCRIPTOR}");
        return Ok(());
    }
    let (mut source, mut fuel, mut dimension) = (None, 1000_u64, 1_usize);
    let mut i = 0;
    while i < args.len() {
        let value = args.get(i + 1).ok_or("missing_argument")?;
        match args[i].as_str() {
            "--eval" => source = Some(value.clone()),
            "--engine" => match value.as_str() {
                "scripted" => {}
                "boa" | "quickjs" => return Err("engine_unavailable".into()),
                _ => return Err("unknown_engine".into()),
            },
            "--fuel" => fuel = value.parse().map_err(|_| "invalid_fuel")?,
            "--dimension" => dimension = value.parse().map_err(|_| "invalid_dimension")?,
            _ => return Err("unknown_argument".into()),
        }
        i += 2;
    }
    let source = source.ok_or("missing_eval")?;
    let provider = MotionJsProvider::new(ScriptedEngine::new(), Quotas::default());
    let request = ExprRequest {
        source: &source,
        revision: 0,
        tick: EvalTick::new(0),
        seed: 0,
        dimension,
        base: None,
    };
    let host: &dyn HostRead = &NoHost;
    let mut fuel = Fuel::new(fuel);
    let result = provider.evaluate(&request, host, &mut fuel, &CancellationToken::default());
    println!("result={result:?}");
    println!("fuel_used={} stats={:?}", fuel.used(), provider.stats());
    println!("retire={:?}", provider.retire());
    Ok(())
}

fn main() {
    if let Err(code) = run() {
        eprintln!("rejected:{code}; consult --descriptor and correct only the input");
        std::process::exit(2)
    }
}
