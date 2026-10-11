//! MotionJS-local consumer. Default engine: the deterministic scripted test double (not JavaScript).
//! With `--features quickjs`: `--engine quickjs` evaluates real JavaScript in the isolated
//! `motion-js-worker` process (must sit next to this executable or one directory above it);
//! `--engine quickjs-unsafe-in-process` is the test-only in-process engine (hostile source can abort
//! this process); it exists only with `--features quickjs,test-faults`, otherwise `engine_unavailable`.
//! `motion-js-consumer --eval "num:3" [--engine scripted|quickjs|quickjs-unsafe-in-process] [--fuel N] [--dimension N]`
use hsk_studio_accord::CancellationToken;
use hsk_studio_motion::{EvalTick, ExprRequest, ExpressionProvider, Fuel, HostRead, NoHost};
use hsk_studio_motion_js::*;
use std::env;

fn evaluate<E: Engine>(engine: E, quotas: Quotas, source: &str, fuel: u64, dimension: usize) {
    let provider = MotionJsProvider::new(engine, quotas);
    let request = ExprRequest {
        source,
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
}

fn run() -> Result<(), String> {
    let args: Vec<_> = env::args().skip(1).collect();
    if args == ["--descriptor"] || args == ["--help"] {
        println!("{DESCRIPTOR}");
        return Ok(());
    }
    let (mut source, mut fuel, mut dimension, mut engine) = (None, 1000_u64, 1_usize, "scripted");
    let mut validate: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        let value = args.get(i + 1).ok_or("missing_argument")?;
        match args[i].as_str() {
            "--eval" => source = Some(value.clone()),
            "--validate" => validate = Some(value.clone()),
            "--engine" => {
                engine = match value.as_str() {
                    "scripted" => "scripted",
                    "quickjs" => "quickjs",
                    "quickjs-unsafe-in-process" => "quickjs-unsafe-in-process",
                    "boa" => return Err("engine_unavailable".into()),
                    _ => return Err("unknown_engine".into()),
                }
            }
            "--fuel" => fuel = value.parse().map_err(|_| "invalid_fuel")?,
            "--dimension" => dimension = value.parse().map_err(|_| "invalid_dimension")?,
            _ => return Err("unknown_argument".into()),
        }
        i += 2;
    }
    if let Some(text) = validate {
        match ProfileValidator::default().validate(&text) {
            Ok(facts) => println!("profile=ok {facts:?}"),
            Err(v) => println!(
                "profile=violation kind={} line={} column={} len={}",
                v.kind.code(),
                v.span.line,
                v.span.column,
                v.len
            ),
        }
        return Ok(());
    }
    let source = source.ok_or("missing_eval")?;
    let quotas = Quotas::default();
    match engine {
        "scripted" => evaluate(ScriptedEngine::new(), quotas, &source, fuel, dimension),
        #[cfg(feature = "quickjs")]
        "quickjs" => {
            let engine = QuickJsBackend::new(&quotas, QuickJsOptions::default()).map_err(|error| error.code().to_owned())?;
            evaluate(engine, quotas, &source, fuel, dimension);
        }
        #[cfg(all(feature = "quickjs", feature = "test-faults"))]
        "quickjs-unsafe-in-process" => {
            let engine = QuickJsBackend::new(&quotas, QuickJsOptions::unsafe_in_process()).map_err(|error| error.code().to_owned())?;
            evaluate(engine, quotas, &source, fuel, dimension);
        }
        _ => return Err("engine_unavailable".into()),
    }
    Ok(())
}

fn main() {
    if let Err(code) = run() {
        eprintln!("rejected:{code}; consult --descriptor and correct only the input");
        std::process::exit(2)
    }
}
