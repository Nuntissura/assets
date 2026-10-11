//! `ScriptedEngine`: a deterministic TEST DOUBLE for the adapter contract. It is NOT a JavaScript
//! interpreter and parses no JavaScript: the "source" is a `;`-separated list of fixed command
//! words, so the provider's quota, fuel, deadline, error-mapping, output-conversion and
//! realm-accounting behaviour can be proven without a native engine. The last command's value is
//! the result. Commands:
//!
//! `num:<f64>` `arr:<f64>,..` `read:<path>[#<tick>]` `readsum:<path>,..` `set:<name>=<f64>`
//! `get:<name>` `date` `spin` `recurse` `alloc:<bytes>` `slow:<ms>` `throw:ref|other@<l>:<c>`
//! `parse@<l>:<c>` `unsupported@<l>:<c>` `random` `obj` `nan` `bool` `null` `big:<n>` `deep:<n>`
//! `internal` `panic` `leak`.
use crate::{
    engine::{
        BudgetKind, Engine, EngineFailure, EngineIdentity, EngineRun, EvalJob, HostBridge, JsValue,
        RealmReceipt, ThrownKind,
    },
    provider::ManualClock,
};
use hsk_studio_motion::{EvalTick, SourceSpan};
use std::{collections::BTreeMap, sync::Arc};

/// Upper bound on scripted loop steps so a misbehaving bridge cannot hang a test.
const STEP_CAP: u64 = 5_000_000;

#[derive(Debug, Default, Clone)]
pub struct ScriptedEngine {
    clock: Arc<ManualClock>,
}

impl ScriptedEngine {
    pub fn new() -> Self {
        Self::default()
    }

    /// `slow:<ms>` advances this clock; share it with the provider via `MotionJsProvider::with_clock`.
    pub fn with_clock(clock: Arc<ManualClock>) -> Self {
        Self { clock }
    }

    pub fn clock(&self) -> Arc<ManualClock> {
        Arc::clone(&self.clock)
    }
}

fn span(text: &str) -> Option<SourceSpan> {
    let (line, column) = text.split_once(':')?;
    Some(SourceSpan { line: line.parse().ok()?, column: column.parse().ok()? })
}

fn number(text: &str) -> Result<f64, EngineFailure> {
    text.trim().parse().map_err(|_| EngineFailure::Parse(Some(SourceSpan { line: 1, column: 1 })))
}

impl ScriptedEngine {
    fn script(
        &self,
        job: &EvalJob<'_>,
        host: &mut dyn HostBridge,
        realm: &mut BTreeMap<String, f64>,
    ) -> Result<JsValue, EngineFailure> {
        let mut value = JsValue::Undefined;
        for op in job.source.split(';') {
            let op = op.trim();
            let (body, at) = op.split_once('@').unwrap_or((op, ""));
            let (name, arg) = body.split_once(':').unwrap_or((body, ""));
            value = match name {
                "" | "leak" => value,
                "num" => JsValue::Number(number(arg)?),
                "arr" => JsValue::Array(
                    arg.split(',').map(|n| number(n).map(JsValue::Number)).collect::<Result<_, _>>()?,
                ),
                "read" => {
                    let (path, tick) = arg.split_once('#').unwrap_or((arg, ""));
                    let tick = tick.parse::<i64>().ok().map(EvalTick::new);
                    let values = host.read(path, tick)?;
                    JsValue::Array(values.into_iter().map(JsValue::Number).collect())
                }
                "readsum" => {
                    let mut total = 0.0;
                    for path in arg.split(',') {
                        total += host.read(path, None)?.first().copied().unwrap_or(0.0);
                    }
                    JsValue::Number(total)
                }
                "set" => {
                    let (key, number_text) = arg.split_once('=').unwrap_or((arg, "0"));
                    realm.insert(key.to_owned(), number(number_text)?);
                    JsValue::Undefined
                }
                "get" => match realm.get(arg) {
                    Some(n) => JsValue::Number(*n),
                    None => {
                        return Err(EngineFailure::Thrown { kind: ThrownKind::Reference, span: None });
                    }
                },
                "date" => JsValue::Number(host.utc_ms()? as f64),
                "spin" => {
                    for _ in 0..STEP_CAP {
                        host.charge(1)?;
                    }
                    return Err(EngineFailure::Internal);
                }
                "recurse" => {
                    for depth in 1..STEP_CAP {
                        host.charge(1)?;
                        if depth > u64::from(job.limits.recursion_limit) {
                            return Err(EngineFailure::Budget(BudgetKind::Recursion));
                        }
                    }
                    return Err(EngineFailure::Internal);
                }
                "alloc" => {
                    let bytes = arg.parse::<u64>().map_err(|_| EngineFailure::Internal)?;
                    if bytes > job.limits.memory_bytes {
                        return Err(EngineFailure::Budget(BudgetKind::Memory));
                    }
                    JsValue::Undefined
                }
                "slow" => {
                    self.clock.advance_ms(arg.parse().map_err(|_| EngineFailure::Internal)?);
                    host.checkpoint()?;
                    value
                }
                "throw" => {
                    let kind = if arg == "ref" { ThrownKind::Reference } else { ThrownKind::Other };
                    return Err(EngineFailure::Thrown { kind, span: span(at) });
                }
                "parse" => return Err(EngineFailure::Parse(span(at))),
                "unsupported" => return Err(EngineFailure::Unsupported(span(at))),
                "random" => {
                    return Err(EngineFailure::Unsupported(Some(SourceSpan { line: 1, column: 1 })));
                }
                "obj" => JsValue::Object,
                "nan" => JsValue::Number(f64::NAN),
                "bool" => JsValue::Bool(true),
                "null" => JsValue::Null,
                // Deliberately unchecked against the output caps: the provider must refuse them.
                "big" => JsValue::Array(vec![JsValue::Number(1.0); arg.parse().unwrap_or(0)]),
                "deep" => (0..arg.parse().unwrap_or(0)).fold(JsValue::Number(1.0), |inner, _| {
                    JsValue::Array(vec![inner])
                }),
                "internal" => return Err(EngineFailure::Internal),
                "panic" => panic!("scripted engine panic"),
                _ => return Err(EngineFailure::Parse(Some(SourceSpan { line: 1, column: 1 }))),
            };
        }
        Ok(value)
    }
}

impl Engine for ScriptedEngine {
    fn identity(&self) -> EngineIdentity {
        EngineIdentity { name: "scripted-double", version: "1", native: false }
    }

    fn run(&self, job: &EvalJob<'_>, host: &mut dyn HostBridge) -> EngineRun {
        // A fresh realm per call, dropped before returning (unless `leak` simulates a broken adapter).
        let leak = job.source.split(';').any(|op| op.trim() == "leak");
        let mut realm = BTreeMap::new();
        let outcome = self.script(job, host, &mut realm);
        drop(realm);
        EngineRun { outcome, realm: RealmReceipt { fresh: true, dropped: !leak } }
    }
}
