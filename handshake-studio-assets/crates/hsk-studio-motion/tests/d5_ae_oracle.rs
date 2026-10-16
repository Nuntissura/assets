//! D5 After Effects oracle comparison (lane SMB-D5-AI-ID-AE-COMPARE): every case of
//! `oracle-session/hsk_oracle_aftereffects.jsx` driven through Studio Motion and compared with the
//! `valueAtTime` samples After Effects 26.5 wrote to `<oracle-corpus>/adobe-session/aftereffects/`.
//!
//! Sample time: AE samples `valueAtTime(t)` at a time quantised to 1/1024 frame (comp 24 fps, so
//! 1/24576 s; measured: t = 0.02 s evaluates as 492/24576 s, linear keys 0 -> 90 over 2 s give
//! 0.900878906 = 90 * 492/49152). Studio is sampled at the same quantised time (nearest tick of the
//! 254016000000 ticks/s clock, error below 2e-12 s); without the quantisation the residual is up to
//! 0.012 degrees, all of it AE's time rounding.
//!
//! Verdicts (one `D5_VERDICT:` line per case variant): MATCH within the stated tolerance, MISMATCH
//! (fails unless listed in `tests/d5_ae_expected_mismatch.txt` with a reason), NOT_SUPPORTED (Studio
//! has no engine for the case; the measured delta is printed), NOT_PROVEN (corpus or variant absent).
mod d5_support;

use d5_support::{J, Ledger, Verdict, corpus_root, max_abs, parse, read_pinned};
use hsk_studio_accord::{CancellationToken, TICKS_PER_SECOND, Ticks};
use hsk_studio_motion::*;

const DIR: &str = "adobe-session/aftereffects";
const PIN_AE01: &str = "e56daf65a3103fd33c0928bef8b7a97743d983bc02d6cfad462b45d2c7da5505";
const PIN_AE01B: &str = "1f934b5cd764489ab57ccc528d7f9fce39c3837c4ada191fefa460acb5cb937d";
const PIN_AE02: &str = "d002f0d37400cdb4945883a889f905834e82f4f5b11bf4601740500f694f9a45";
const PIN_AE03: &str = "2dc73a7c865a33cf613486e6e9674b182352a48046d177be58de3b71d030f246";
const EXPECTED: &str = include_str!("d5_ae_expected_mismatch.txt");

/// Value tolerance of a scalar (degrees) comparison: the oracle prints 9 decimals.
const TOL_DEG: f64 = 1e-6;
/// Position tolerance (pixels) for spatial streams: AE measures path length with its own arc-length
/// table; on a straight path the residual is 0.0037 px of a 288 px path.
const TOL_PX: f64 = 0.01;
/// AE sample-time quantum: 1/1024 frame at 24 fps.
const AE_TIME_QUANTA_PER_SECOND: i64 = 24 * 1024;
const S: i64 = TICKS_PER_SECOND as i64;

/// Tick of `t = num / den` seconds after AE's 1/24576 s quantisation (nearest; no ties occur for the
/// oracle's sample times, see module docs).
fn ae_tick(num: i64, den: i64) -> i64 {
    let q = (2 * num * AE_TIME_QUANTA_PER_SECOND + den) / (2 * den);
    let ticks = (i128::from(q) * i128::from(S) * 2 + i128::from(AE_TIME_QUANTA_PER_SECOND)) / (2 * i128::from(AE_TIME_QUANTA_PER_SECOND));
    i64::try_from(ticks).expect("tick fits")
}

fn tangent(speed: f64, influence_percent: f64) -> TemporalTangent {
    TemporalTangent::new(speed, influence_percent / 100.0).expect("oracle tangent in range")
}

fn bez_key(seconds: i64, value: &[f64], t: TemporalTangent) -> Keyframe {
    Keyframe::new(Ticks::new((seconds * S) as u64), value.to_vec())
        .with_interpolation(Interpolation::Bezier, Interpolation::Bezier)
        .with_tangents(vec![t], vec![t])
}

/// 101 samples `t0 + dt * k / 100` (component `c`), AE-quantised.
fn samples(p: &Property, dt_seconds: i64, c: usize) -> Vec<f64> {
    let cancel = CancellationToken::default();
    (0..=100)
        .map(|k| p.sample(EvalTick::new(ae_tick(dt_seconds * k, 100)), &cancel).expect("sample")[c])
        .collect()
}

fn load(test: &'static str, file: &str, pin: &str) -> Option<String> {
    let Some(root) = corpus_root() else {
        println!("NOT_PROVEN: {test} reason=oracle-corpus root not found via .config/oracle-corpus.toml");
        return None;
    };
    match read_pinned(&root, DIR, file, pin) {
        Ok(bytes) => Some(String::from_utf8(bytes).expect("oracle utf8")),
        Err(reason) => {
            println!("NOT_PROVEN: {test} reason={reason}");
            None
        }
    }
}

fn field(row: &J, key: &str) -> f64 {
    row.field_num(key).unwrap_or_else(|| panic!("oracle field {key}"))
}

fn values(row: &J) -> Vec<f64> {
    row.get("v").and_then(J::nums).expect("oracle v[]")
}

fn verdict(err: f64, tol: f64) -> Verdict {
    if err <= tol { Verdict::Match } else { Verdict::Mismatch }
}

/// AE-01: 324 two-key Bezier segments (speed x influence x duration x direction) on Rotation.
#[test]
fn ae01_bezier_grid() {
    const T: &str = "d5_ae_oracle::ae01_bezier_grid";
    let Some(text) = load(T, "AE-01__bezier_grid.jsonl", PIN_AE01) else { return };
    let mut lines = text.lines();
    let header = parse(lines.next().expect("header")).expect("header json");
    assert_eq!(header.field_num("n_samples_per_segment"), Some(101.0));
    let mut ledger = Ledger::new("d5_ae_oracle::ae01_bezier_grid", EXPECTED);
    let mut worst = 0.0f64;
    let mut segments = 0usize;
    for line in lines.filter(|l| !l.trim().is_empty()) {
        let row = parse(line).expect("segment json");
        let seg = field(&row, "seg");
        let dt = field(&row, "dt");
        assert!(dt == dt.trunc() && dt > 0.0, "whole-second dt");
        let dt = dt as i64;
        let keys = vec![
            bez_key(0, &[0.0], tangent(field(&row, "s0"), field(&row, "i0"))),
            bez_key(dt, &[field(&row, "dv")], tangent(field(&row, "s1"), field(&row, "i1"))),
        ];
        let p = Property::new("rotation", vec![0.0], keys).expect("studio property");
        let err = max_abs(&samples(&p, dt, 0), &values(&row)).expect("101 samples");
        worst = worst.max(err);
        segments += 1;
        ledger.record("AE-01", &format!("seg{seg}"), verdict(err, TOL_DEG), format!("max_abs_deg={err:.3e} tol={TOL_DEG:e}"));
    }
    assert_eq!(segments, 324, "AE-01 segment count");
    println!("AE-01 worst max_abs_deg={worst:.3e} over {segments} segments");
    ledger.finish();
}

fn interp(name: &str) -> Interpolation {
    match name {
        "LINEAR" => Interpolation::Linear,
        "BEZIER" => Interpolation::Bezier,
        "HOLD" => Interpolation::Hold,
        other => panic!("oracle interpolation {other}"),
    }
}

fn ease_of(row: &J, key: &str) -> Option<TemporalTangent> {
    let e = row.get(key)?;
    if e.is_null() {
        return None;
    }
    let v = e.nums()?;
    Some(tangent(v[0], v[1]))
}

/// AE-01b: LINEAR/BEZIER/HOLD departure x arrival pairs and 3-key chains with the middle key plain,
/// auto bezier (Studio `auto_bezier`) or continuous (Studio `make_continuous`).
#[test]
fn ae01b_mixed_types_and_chains() {
    const T: &str = "d5_ae_oracle::ae01b_mixed_types_and_chains";
    let Some(text) = load(T, "AE-01b__mixed_types.json", PIN_AE01B) else { return };
    let doc = parse(&text).expect("AE-01b json");
    let rows = doc.get("rows").and_then(J::arr).expect("rows");
    let mut ledger = Ledger::new("d5_ae_oracle::ae01b_mixed_types_and_chains", EXPECTED);
    let (mut pairs, mut chains) = (0, 0);
    for row in rows {
        if let Some(out_type) = row.get("out_type_k1").and_then(J::str) {
            let in_type = row.get("in_type_k2").and_then(J::str).expect("in_type_k2");
            let (e1, e2) = (ease_of(row, "ease_k1"), ease_of(row, "ease_k2"));
            // setInterpolationTypeAtKey(1, LINEAR, out) / (2, in, LINEAR); ease on both sides of a key.
            let mut k1 = Keyframe::new(Ticks::new(0), vec![0.0]).with_interpolation(Interpolation::Linear, interp(out_type));
            if let Some(e) = e1 {
                k1 = k1.with_tangents(vec![e], vec![e]);
            }
            let mut k2 = Keyframe::new(Ticks::new((2 * S) as u64), vec![90.0]).with_interpolation(interp(in_type), Interpolation::Linear);
            if let Some(e) = e2 {
                k2 = k2.with_tangents(vec![e], vec![e]);
            }
            let p = Property::new("rotation", vec![0.0], vec![k1, k2]).expect("studio property");
            let err = max_abs(&samples(&p, 2, 0), &values(row)).expect("101 samples");
            ledger.record("AE-01b", &format!("{out_type}_{in_type}"), verdict(err, TOL_DEG), format!("max_abs_deg={err:.3e}"));
            pairs += 1;
            continue;
        }
        let chain = row.get("chain3").and_then(J::str).expect("chain3 row");
        let read = row.get("ease_k2_read").and_then(J::nums).expect("ease_k2_read");
        // setValueAtTime x3 (linear), then BEZIER on every side: Studio's set_interpolation stores
        // the default ease (speed 0, influence 0.16666666666) where a side has no tangent.
        let linear = |s: i64, v: f64| Keyframe::new(Ticks::new((s * S) as u64), vec![v]);
        let mut p = Property::new("rotation", vec![0.0], vec![linear(0, 0.0), linear(2, 90.0), linear(4, 30.0)]).expect("chain");
        set_interpolation(&mut p, &[0, 1, 2], EaseSide::Both, Interpolation::Bezier).expect("bezier");
        let record = VelocityRecord { incoming: vec![tangent(20.0, 40.0)], outgoing: vec![tangent(60.0, 25.0)], continuous: false };
        set_velocity(&mut p, 1, &record).expect("setTemporalEaseAtKey(2, ...)");
        match chain {
            "plain" => {}
            "auto_bezier" => auto_bezier(&mut p, &[1]).expect("auto bezier"),
            "continuous" => make_continuous(&mut p, &[1]).expect("continuous"),
            other => panic!("chain {other}"),
        }
        let k = &p.keyframes()[1];
        let studio_read = [k.in_tangent[0].speed, k.in_tangent[0].influence * 100.0, k.out_tangent[0].speed, k.out_tangent[0].influence * 100.0];
        let read_err = max_abs(&studio_read, &read).expect("4 ease numbers");
        let err = max_abs(&samples(&p, 4, 0), &values(row)).expect("101 samples");
        let v = if read_err <= 1e-6 { verdict(err, TOL_DEG) } else { Verdict::Mismatch };
        ledger.record("AE-01b", &format!("chain3_{chain}"), v, format!("max_abs_deg={err:.3e} ease_read_err={read_err:.3e} studio_ease={studio_read:?}"));
        chains += 1;
    }
    assert_eq!((pairs, chains), (6, 3), "AE-01b row count");
    ledger.finish();
}

/// AE-02: 2-D Position. Studio has no spatial tangents (STU-MOT-036b gap): a straight path with a
/// single path-speed ease is evaluated per component; curved spatial paths are NOT_SUPPORTED.
#[test]
fn ae02_spatial_position() {
    const T: &str = "d5_ae_oracle::ae02_spatial_position";
    let Some(text) = load(T, "AE-02__spatial.json", PIN_AE02) else { return };
    let doc = parse(&text).expect("AE-02 json");
    let rows = doc.get("rows").and_then(J::arr).expect("rows");
    let mut ledger = Ledger::new("d5_ae_oracle::ae02_spatial_position", EXPECTED);
    let find = |name: &str| rows.iter().find(|r| r.get("variant").and_then(J::str) == Some(name));
    let points = |row: &J| -> Vec<[f64; 2]> {
        row.get("v").and_then(J::arr).expect("v").iter().map(|p| {
            let v = p.nums().expect("point");
            [v[0], v[1]]
        }).collect()
    };
    let cancel = CancellationToken::default();
    match find("two_keys_default_spatial") {
        Some(row) => {
            assert_eq!(row.field_num("ease_array_length_out_k1"), Some(1.0), "one spatial ease");
            let ease = tangent(0.0, 16.666666667);
            let p = Property::new("position", vec![40.0, 200.0], vec![bez_key(0, &[40.0, 200.0], ease), bez_key(2, &[280.0, 40.0], ease)]).expect("position");
            let oracle = points(row);
            let err = (0..=100i64)
                .map(|k| {
                    let v = p.sample(EvalTick::new(ae_tick(2 * k, 100)), &cancel).expect("sample");
                    let o = oracle[k as usize];
                    ((v[0] - o[0]).powi(2) + (v[1] - o[1]).powi(2)).sqrt()
                })
                .fold(0.0, f64::max);
            ledger.record("AE-02", "two_keys_default_spatial", verdict(err, TOL_PX), format!("max_dist_px={err:.3e} tol={TOL_PX}"));
        }
        None => ledger.record("AE-02", "two_keys_default_spatial", Verdict::NotProven, "variant absent from corpus".into()),
    }
    match find("two_keys_spatial_tangents_set") {
        Some(_) => ledger.record("AE-02", "two_keys_spatial_tangents_set", Verdict::NotSupported, "Studio Motion has no spatial tangents (STU-MOT-036b)".into()),
        None => ledger.record("AE-02", "two_keys_spatial_tangents_set", Verdict::NotProven,
            "variant absent from corpus (session script passed 2-element tangents to 3-D Position; script fixed, AE rerun pending)".into()),
    }
    match find("three_keys_roving_middle") {
        Some(row) => {
            let lin = |s: i64, v: [f64; 2]| Keyframe::new(Ticks::new((s * S) as u64), v.to_vec());
            let mut p = Property::new("position", vec![40.0, 200.0], vec![lin(0, [40.0, 200.0]), lin(1, [100.0, 60.0]), lin(3, [280.0, 40.0])]).expect("position");
            let times = rove(&mut p, &[false, true, false]).expect("rove");
            let studio_t = times[1].value() as f64 / S as f64;
            let ae_t = row.field_num("time_k2_after_roving").expect("time_k2_after_roving");
            let oracle = points(row);
            let dist = (0..=100i64)
                .map(|k| {
                    let v = p.sample(EvalTick::new(ae_tick(3 * k, 100)), &cancel).expect("sample");
                    let o = oracle[k as usize];
                    ((v[0] - o[0]).powi(2) + (v[1] - o[1]).powi(2)).sqrt()
                })
                .fold(0.0, f64::max);
            // AE roves along its spatial auto-bezier/continuous path (curved arc length); Studio
            // roves by straight-line distance because it has no spatial interpolation.
            ledger.record("AE-02", "three_keys_roving_middle", Verdict::NotSupported,
                format!("spatial auto-bezier path absent (STU-MOT-036b); rove time studio={studio_t:.6}s ae={ae_t:.6}s delta={:.4}s; max_dist_px={dist:.3}", studio_t - ae_t));
        }
        None => ledger.record("AE-02", "three_keys_roving_middle", Verdict::NotProven, "variant absent from corpus".into()),
    }
    ledger.finish();
}

/// AE-03: expressions. `valueAtTime(t, true)` (pre-expression) is the keyframed value; the
/// post-expression values need After Effects' expression globals (`value`, `time`, `linear()`),
/// which the Studio expression profile does not bind: the property reports `unsupported`.
#[test]
fn ae03_expressions() {
    const T: &str = "d5_ae_oracle::ae03_expressions";
    let Some(text) = load(T, "AE-03__expressions.json", PIN_AE03) else { return };
    let doc = parse(&text).expect("AE-03 json");
    let rows = doc.get("rows").and_then(J::arr).expect("rows");
    assert_eq!(rows.len(), 3, "AE-03 rows");
    let mut ledger = Ledger::new("d5_ae_oracle::ae03_expressions", EXPECTED);
    let lin = |s: i64, v: f64| Keyframe::new(Ticks::new((s * S) as u64), vec![v]);
    for row in rows {
        let expr = row.get("expression").and_then(J::str).expect("expression");
        let mut p = Property::new("rotation", vec![0.0], vec![lin(0, 0.0), lin(2, 90.0)])
            .expect("rotation")
            .with_expression(expr)
            .expect("expression accepted");
        p.set_expression_enabled(true);
        let pre = row.get("pre_expression_true").and_then(J::nums).expect("pre");
        let err = max_abs(&samples(&p, 2, 0), &pre).expect("101 samples");
        ledger.record("AE-03", &format!("pre_expression[{expr}]"), verdict(err, TOL_DEG), format!("max_abs_deg={err:.3e}"));
        let ctx = EvalContext { tick: EvalTick::new(ae_tick(1, 1)), seed: 0, expected_revision: p.revision() };
        let out = p.evaluate_pure(&ctx, &CancellationToken::default()).expect("evaluation");
        assert_eq!(out.receipt.disabled_by_error, Some(ExprErrorCode::Unsupported), "typed unsupported outcome");
        let post = row.get("pre_expression_false").and_then(J::nums).expect("post");
        ledger.record("AE-03", &format!("post_expression[{expr}]"), Verdict::NotSupported,
            format!("no AE expression globals (value/time/linear) in the Studio profile; receipt disabled_by_error=unsupported; AE at t=1s gives {:.6}", post[50]));
    }
    ledger.finish();
}
