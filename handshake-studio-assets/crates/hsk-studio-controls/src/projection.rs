//! Projections of the SAME descriptors: model-navigable JSON for agents (and its parser) and
//! UserManual topics. Nothing here carries independent control semantics.

use crate::descriptor::{ControlKind, ControlSpec, NumberSpec};
use crate::registry::Registry;
use crate::store::ValueStore;
use crate::value::ControlError;
use serde_json::{Map, Value as Json};

pub const SCHEMA: &str = "hsk.studio.controls@1";

fn actions(spec: &ControlSpec) -> Vec<&'static str> {
    match spec.kind {
        ControlKind::Number(_) => vec!["set", "step_up", "step_down", "pointer", "reset"],
        _ => vec!["set", "reset"],
    }
}

fn to_json<T: serde::Serialize>(v: &T) -> Json {
    serde_json::to_value(v).unwrap_or(Json::Null)
}

fn control_json(registry: &Registry, spec: &ControlSpec, store: Option<&ValueStore>) -> Json {
    let mut v = to_json(spec);
    if let (Json::Object(obj), ControlKind::Number(n)) = (&mut v, &spec.kind)
        && let Some(Json::Object(kind)) = obj.get_mut("kind")
    {
        kind.insert("step".into(), to_json(&n.step()));
        kind.insert("fine_step".into(), to_json(&n.fine_step()));
        kind.insert("coarse_step".into(), to_json(&n.coarse_step()));
    }
    if let Json::Object(obj) = &mut v {
        obj.insert("consumers".into(), to_json(&registry.consumers_of(&spec.id)));
        obj.insert("actions".into(), to_json(&actions(spec)));
        if let Some(value) = store.and_then(|s| s.get(&spec.id)) {
            obj.insert("value".into(), to_json(value));
        }
    }
    v
}

/// Deterministic pretty JSON for agents: every descriptor with all four bounds, the derived step
/// trio, help text, consumers, allowed actions and (when a store is given) the current value.
pub fn agent_json(registry: &Registry, store: Option<&ValueStore>) -> String {
    let controls: Vec<Json> = registry.specs().iter().map(|s| control_json(registry, s, store)).collect();
    let mut root = Map::new();
    root.insert("schema".into(), Json::String(SCHEMA.to_string()));
    root.insert("controls".into(), Json::Array(controls));
    serde_json::to_string_pretty(&Json::Object(root)).unwrap_or_default()
}

fn bad_json(reason: impl Into<String>) -> ControlError {
    ControlError::InvalidDescriptor { control_id: "<controls-json>".to_string(), reason: reason.into() }
}

/// Rebuilds a registry (descriptors and consumers) from [`agent_json`] output. Derived fields
/// (steps, value, actions) are ignored; descriptors are fully re-validated on registration.
pub fn registry_from_json(json: &str) -> Result<Registry, ControlError> {
    let root: Json = serde_json::from_str(json).map_err(|e| bad_json(e.to_string()))?;
    if root.get("schema").and_then(Json::as_str) != Some(SCHEMA) {
        return Err(bad_json(format!("schema must be `{SCHEMA}`")));
    }
    let controls = root.get("controls").and_then(Json::as_array).ok_or_else(|| bad_json("missing `controls` array"))?;
    let mut registry = Registry::new();
    for c in controls {
        let spec: ControlSpec = serde_json::from_value(c.clone()).map_err(|e| bad_json(e.to_string()))?;
        let id = spec.id.clone();
        registry.register(spec)?;
        for consumer in c.get("consumers").and_then(Json::as_array).into_iter().flatten() {
            let name = consumer.as_str().ok_or_else(|| bad_json("consumer must be a string"))?;
            registry.register_consumer(&id, name)?;
        }
    }
    Ok(registry)
}

/// One UserManual topic derived from a descriptor (topic id = control id).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManualTopic {
    pub id: String,
    pub title: String,
    pub body: String,
}

fn bound(b: Option<f64>) -> String {
    b.map_or_else(|| "none".to_string(), |v| v.to_string())
}

fn number_lines(spec: &ControlSpec, n: &NumberSpec) -> String {
    format!(
        "Hard range: [{}, {}] (typed values outside are rejected, never clamped).\n\
         Soft range: [{}, {}] (presentation and slider travel; values beyond it are legal and flagged).\n\
         Precision: {} decimals; step {}, fine step {}, coarse step {}.{}",
        bound(n.hard_min),
        bound(n.hard_max),
        bound(n.soft_min),
        bound(n.soft_max),
        n.precision,
        n.step(),
        n.fine_step(),
        n.coarse_step(),
        spec.unit.as_ref().map(|u| format!("\nUnit: {u}.")).unwrap_or_default(),
    )
}

pub fn manual_topics(registry: &Registry) -> Vec<ManualTopic> {
    registry
        .specs()
        .iter()
        .map(|spec| {
            let detail = match &spec.kind {
                ControlKind::Number(n) => number_lines(spec, n),
                ControlKind::Toggle => "On or off.".to_string(),
                ControlKind::Choice { options } => format!(
                    "One of: {}.",
                    options.iter().map(|o| format!("{} (`{}`)", o.label, o.id)).collect::<Vec<_>>().join(", ")
                ),
                ControlKind::Color => "Colour as #rrggbb or #rrggbbaa (sRGB).".to_string(),
                ControlKind::Text { max_chars, multiline } => {
                    format!("Text up to {max_chars} characters{}.", if *multiline { ", multiple lines allowed" } else { "" })
                }
            };
            ManualTopic {
                id: spec.id.clone(),
                title: spec.label.clone(),
                body: format!("{}\n{}\nDefault: {}.", spec.help, detail, spec.format_value(&spec.default)),
            }
        })
        .collect()
}
