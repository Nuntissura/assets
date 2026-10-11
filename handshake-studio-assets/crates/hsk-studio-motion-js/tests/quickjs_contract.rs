#![cfg(feature = "quickjs")]
//! QuickJS backend contract (run with `--features quickjs`).
use hsk_studio_accord::CancellationToken;
use hsk_studio_motion::*;
use hsk_studio_motion_js::*;
use std::{sync::Arc, time::Duration};

struct Host;

impl HostRead for Host {
    fn read(&self, path: &PropertyPath, tick: EvalTick, fuel: &mut Fuel) -> Result<Vec<f64>, ExprError> {
        fuel.charge(1)?;
        match path.as_str() {
            "comp/layer/transform/position" => Ok(vec![960.0, 540.0]),
            "comp/a" => Ok(vec![2.0]),
            "comp/b" => Ok(vec![3.0]),
            "tick/x" => Ok(vec![tick.value() as f64]),
            "amb" => Err(ExprError::new(ExprErrorCode::Ambiguous)),
            _ => Err(ExprError::new(ExprErrorCode::ReferenceMissing)),
        }
    }
}

type P = MotionJsProvider<QuickJsEngine>;

fn provider_with(spec: QuotaSpec) -> P {
    let quotas = Quotas::try_from_spec(spec).unwrap();
    MotionJsProvider::new(QuickJsEngine::new(&quotas).unwrap(), quotas)
}

fn eval(provider: &P, source: &str, dimension: usize) -> Result<Vec<f64>, ExprError> {
    let request = ExprRequest { source, revision: 0, tick: EvalTick::new(0), seed: 0, dimension, base: None };
    provider.evaluate(&request, &Host, &mut Fuel::new(1000), &CancellationToken::default())
}

#[test]
fn probe_print() {
    let provider = provider_with(QuotaSpec::default());
    for (source, dim) in [
        ("1+2", 1), ("1 +", 1), ("undefinedName", 1), ("throw new Error('x')", 1), ("with(Math){}", 1),
        ("Math.random()", 1), ("eval('1')", 1), ("Function('return 1')()", 1), ("(()=>{}).constructor('return 1')()", 1),
        ("function f(){return f()+1} f()", 1), ("'x'.repeat(100000000).length", 1), ("typeof Date === 'undefined' ? 1 : 0", 1),
        ("[1,2]", 2), ("({})", 1), ("hostRead('comp/a')[0]", 1), ("import('x')", 1), ("throw 5", 1), ("(async function(){}).constructor('return 1')", 1),
        ("(function*(){}).constructor('yield 1')", 1), ("typeof Promise + typeof Proxy + typeof Reflect + typeof JSON + typeof RegExp", 1),
    ] {
        println!("{source:?} -> {:?}", eval(&provider, source, dim));
    }
    println!("{:?}", provider.stats());
}
