#![cfg(feature = "quickjs")]
//! D5 After Effects oracle, case AE-03 post-expression values (lane SMB-AI-AE-ORACLE-FOLLOWUPS
//! slice 4): the three expressions of `oracle-session/hsk_oracle_aftereffects.jsx` evaluated by the
//! QuickJS provider (worker process) with the After Effects globals `value`, `time` and `linear()`
//! (`AE_GLOBALS_PRELUDE`), compared with `valueAtTime(t, false)` that After Effects 26.5 wrote to
//! `<oracle-corpus>/adobe-session/aftereffects/AE-03__expressions.json`. The pre-expression values of
//! the same file stay in `hsk-studio-motion` `d5_ae_oracle::ae03_expressions` (pure Motion has no
//! engine). Sample times are After Effects' 1/1024-frame quantised times (see that test's docs).
//! Verdicts print as `D5_VERDICT:`; a MISMATCH fails (no expected mismatches for this case).
#[path = "../../hsk-studio-render-cpu/tests/d5_support/mod.rs"]
mod d5_support;

use d5_support::{J, Ledger, Verdict, corpus_root, max_abs, parse, read_pinned};
use hsk_studio_accord::{CancellationToken, TICKS_PER_SECOND, Ticks};
use hsk_studio_motion::*;
use hsk_studio_motion_js::*;

const PIN_AE03: &str = "2dc73a7c865a33cf613486e6e9674b182352a48046d177be58de3b71d030f246";
const TOL_DEG: f64 = 1e-6;
const AE_TIME_QUANTA_PER_SECOND: i64 = 24 * 1024;
const S: i64 = TICKS_PER_SECOND as i64;

/// Tick of `num / den` seconds after After Effects' 1/24576 s quantisation (as d5_ae_oracle).
fn ae_tick(num: i64, den: i64) -> i64 {
    let q = (2 * num * AE_TIME_QUANTA_PER_SECOND + den) / (2 * den);
    let ticks = (i128::from(q) * i128::from(S) * 2 + i128::from(AE_TIME_QUANTA_PER_SECOND)) / (2 * i128::from(AE_TIME_QUANTA_PER_SECOND));
    i64::try_from(ticks).expect("tick fits")
}

#[test]
fn ae03_post_expression_values() {
    const T: &str = "d5_ae_expressions::ae03_post_expression_values";
    let Some(root) = corpus_root() else {
        println!("NOT_PROVEN: {T} reason=oracle-corpus root not found via .config/oracle-corpus.toml");
        return;
    };
    let text = match read_pinned(&root, "adobe-session/aftereffects", "AE-03__expressions.json", PIN_AE03) {
        Ok(bytes) => String::from_utf8(bytes).expect("oracle utf8"),
        Err(reason) => {
            println!("NOT_PROVEN: {T} reason={reason}");
            return;
        }
    };
    let doc = parse(&text).expect("AE-03 json");
    let rows = doc.get("rows").and_then(J::arr).expect("rows");
    assert_eq!(rows.len(), 3, "AE-03 rows");
    let quotas = Quotas::try_from_spec(QuotaSpec::default()).expect("quotas");
    let options = QuickJsOptions::default().with_worker_program(env!("CARGO_BIN_EXE_motion-js-worker"));
    let provider = MotionJsProvider::new(QuickJsBackend::new(&quotas, options).expect("quickjs backend"), quotas);
    let cancel = CancellationToken::default();
    let mut ledger = Ledger::new(T, "");
    let lin = |s: i64, v: f64| Keyframe::new(Ticks::new((s * S) as u64), vec![v]);
    for row in rows {
        let expr = row.get("expression").and_then(J::str).expect("expression");
        assert_eq!(row.get("expression_error").and_then(J::str), Some(""), "After Effects accepted {expr}");
        let post = row.get("pre_expression_false").and_then(J::nums).expect("post");
        let mut p = Property::new("rotation", vec![0.0], vec![lin(0, 0.0), lin(2, 90.0)])
            .and_then(|p| p.with_expression(expr))
            .expect("expression property");
        let mut studio = Vec::with_capacity(101);
        let mut disabled = None;
        for k in 0..=100 {
            let ctx = EvalContext { expected_revision: p.revision(), tick: EvalTick::new(ae_tick(2 * k, 100)), seed: 0 };
            let out = p.evaluate_with(&ctx, &provider, &NoHost, &mut Fuel::new(1000), &cancel).expect("evaluation");
            disabled = disabled.or(out.receipt.disabled_by_error);
            studio.push(out.value[0]);
        }
        let err = max_abs(&studio, &post).expect("101 samples");
        let v = if disabled.is_none() && err <= TOL_DEG { Verdict::Match } else { Verdict::Mismatch };
        ledger.record("AE-03", &format!("post_expression[{expr}]"), v, format!("max_abs_deg={err:.3e} tol={TOL_DEG:e} disabled_by_error={disabled:?}"));
    }
    ledger.finish();
}
