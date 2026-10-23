//! Motion-local consumer: samples a keyframed property at one tick and prints value + receipt.
//! `motion-consumer --keys "tick:value[/value..]:in:out,..." --tick N [--seed N]`
//! `in`/`out`: linear | bezier | hold (bezier uses the default ease: speed 0, influence 0.16666666666).
use hsk_studio_accord::{CancellationToken, Ticks};
use hsk_studio_motion::*;
use std::env;

fn parse_interp(text: &str) -> Result<Interpolation, &'static str> {
    match text {
        "linear" | "bezier" | "hold" => Interpolation::parse(text).ok_or("invalid_interpolation"),
        _ => Err("invalid_interpolation"),
    }
}

fn parse_keys(text: &str) -> Result<Vec<Keyframe>, &'static str> {
    let mut keys = Vec::new();
    for item in text.split(',') {
        let parts: Vec<&str> = item.split(':').collect();
        let [tick, values, in_text, out_text] = parts[..] else {
            return Err("invalid_key_spec");
        };
        let tick: u64 = tick.parse().map_err(|_| "invalid_tick")?;
        let value = values
            .split('/')
            .map(|v| v.parse::<f64>().map_err(|_| "invalid_value"))
            .collect::<Result<Vec<_>, _>>()?;
        let (in_interp, out_interp) = (parse_interp(in_text)?, parse_interp(out_text)?);
        let tangents = |interp: Interpolation| {
            if interp.is_bezier() {
                vec![TemporalTangent::EASE]
            } else {
                Vec::new()
            }
        };
        keys.push(
            Keyframe::new(Ticks::new(tick), value)
                .with_interpolation(in_interp, out_interp)
                .with_tangents(tangents(in_interp), tangents(out_interp)),
        );
    }
    Ok(keys)
}

fn run() -> Result<(), String> {
    let args: Vec<_> = env::args().skip(1).collect();
    if args == ["--descriptor"] || args == ["--help"] {
        println!("{DESCRIPTOR}");
        return Ok(());
    }
    let (mut keys, mut tick, mut seed) = (None, None, 0_u64);
    let mut i = 0;
    while i < args.len() {
        let value = args.get(i + 1).ok_or("missing_argument")?;
        match args[i].as_str() {
            "--keys" => keys = Some(parse_keys(value)?),
            "--tick" => tick = Some(value.parse::<i64>().map_err(|_| "invalid_tick")?),
            "--seed" => seed = value.parse::<u64>().map_err(|_| "invalid_seed")?,
            _ => return Err("unknown_argument".into()),
        }
        i += 2;
    }
    let keys = keys.ok_or("missing_keys")?;
    let tick = EvalTick::new(tick.ok_or("missing_tick")?);
    let dimension = keys.first().map_or(1, |key| key.value.len());
    let mut property =
        Property::new("value", vec![0.0; dimension], keys).map_err(|e| e.code().to_owned())?;
    let cancel = CancellationToken::default();
    let ctx = EvalContext {
        expected_revision: property.revision(),
        tick,
        seed,
    };
    let evaluation = property
        .evaluate_pure(&ctx, &cancel)
        .map_err(|e| e.code().to_owned())?;
    println!("value={:?}", evaluation.value);
    println!("receipt={:?}", evaluation.receipt);
    Ok(())
}

fn main() {
    if let Err(code) = run() {
        eprintln!("rejected:{code}; consult --descriptor and correct only the input");
        std::process::exit(2)
    }
}
