use handpick::*;
use serde_json::{json, Value};
use std::collections::HashMap;

fn resolve(value: &Value, vars: &HashMap<String, Value>) -> Value {
    match value {
        Value::String(s) if s.starts_with('$') => {
            vars.get(&s[1..]).expect("fixture variable").clone()
        }
        Value::Array(values) => {
            Value::Array(values.iter().map(|value| resolve(value, vars)).collect())
        }
        Value::Object(values) => Value::Object(
            values
                .iter()
                .map(|(key, value)| (key.clone(), resolve(value, vars)))
                .collect(),
        ),
        value => value.clone(),
    }
}
fn run(session: &mut WritingSession, op: &str, args: &[Value]) -> Result<Value, Error> {
    let text = |index: usize| args[index].as_str().unwrap();
    let decode = |index: usize| args[index].clone();
    match op {
        "bind" => session.bind(EditorId::new(text(0)).unwrap(), text(1).parse().unwrap()).map(|_| Value::Null),
        "fence" => session.fence().map(|value| json!(value)),
        "edited" => session.edited(&serde_json::from_value(decode(0)).unwrap(), text(1).parse().unwrap()).map(|_| Value::Null),
        "expand" => session.expand().map(|_| Value::Null),
        "expanded" => Ok(json!(session.expanded())),
        "park" => session.park(PinId::new(text(0)).unwrap()).map(|_| Value::Null),
        "restore" => session.restore(&PinId::new(text(0)).unwrap(), args[1].as_str().map(|value| PinId::new(value).unwrap())).map(|_| Value::Null),
        "pins" => Ok(json!(session.pins().iter().map(|pin| json!({"id":pin.id.as_str(),"editor":pin.editor.as_str(),"revision":pin.revision.to_string()})).collect::<Vec<_>>())),
        "beginLookup" => session.begin_lookup().map(|value| json!(value)),
        "beginComposition" => session.begin_composition(&serde_json::from_value(decode(0)).unwrap()).map(|value| json!(value)),
        "endComposition" => session.end_composition(&serde_json::from_value(decode(0)).unwrap()).map(|_| Value::Null),
        "submit" => session.submit(serde_json::from_value(decode(0)).unwrap()).map(|_| Value::Null),
        "pendingSave" => Ok(json!(session.pending_save())),
        "acknowledge" => session.acknowledge(&serde_json::from_value(decode(0)).unwrap()).map(|value| json!(match value {
            SaveDisposition::CurrentRevisionCommitted => "current_revision_committed",
            SaveDisposition::OlderRevisionCommitted => "older_revision_committed",
            SaveDisposition::RetainActiveComposition => "retain_active_composition",
            SaveDisposition::RetainForRetry => "retain_for_retry",
            SaveDisposition::ReconcileBeforeRetry => "reconcile_before_retry",
        })),
        "deliver" => session.deliver(&serde_json::from_value(decode(0)).unwrap(), serde_json::from_value(decode(1)).unwrap(), serde_json::from_value(decode(2)).unwrap(), serde_json::from_value(decode(3)).unwrap()).map(|_| Value::Null),
        "select" => session.select(&serde_json::from_value(decode(0)).unwrap(), serde_json::from_value(decode(1)).unwrap()).map(|_| Value::Null),
        "selected" => Ok(json!(session.selected())),
        "canExpand" => Ok(json!(session.can_expand())),
        "expandIfEmpty" => session.expand_if_empty(&serde_json::from_value(decode(0)).unwrap()).map(|value| json!(value)),
        "expandForProse" => session.expand_for_prose(&serde_json::from_value(decode(0)).unwrap(), args[1].as_u64().unwrap().try_into().unwrap(), args[2].as_u64().unwrap().try_into().unwrap()).map(|value| json!(value)),
        "compact" => session.compact().map(|_| Value::Null),
        "revoke" => { session.revoke(); Ok(Value::Null) },
        _ => panic!("unknown fixture operation"),
    }
}

#[test]
fn writing_shared_native_browser_fixtures() {
    let fixtures: Value =
        serde_json::from_str(include_str!("fixtures/writing-bridge-v1.json")).unwrap();
    assert_eq!(fixtures["version"], "handpick.v1");
    let scenarios = fixtures["scenarios"].as_array().unwrap();
    assert!(!scenarios.is_empty());
    for scenario in scenarios {
        let mut session = WritingSession::new(
            ContextId::new(scenario["context"].as_str().unwrap()).unwrap(),
            SessionId::new(scenario["session"].as_str().unwrap()).unwrap(),
            scenario["max_pins"].as_u64().unwrap() as usize,
        );
        let mut vars = HashMap::new();
        for step in scenario["steps"].as_array().unwrap() {
            let args = resolve(step.get("args").unwrap_or(&json!([])), &vars);
            let result = run(
                &mut session,
                step["op"].as_str().unwrap(),
                args.as_array().unwrap(),
            );
            if let Some(error) = step.get("error") {
                assert_eq!(result.unwrap_err().to_string(), error.as_str().unwrap());
            } else {
                let value = result.unwrap();
                if let Some(expected) = step.get("expect") {
                    assert_eq!(&value, expected, "{}: {}", scenario["name"], step["op"]);
                }
                if let Some(store) = step.get("store") {
                    vars.insert(store.as_str().unwrap().into(), value);
                }
            }
        }
    }
}
