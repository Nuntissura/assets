//! Projections of the SAME descriptors: model-navigable JSON for agents (and its parser) and
//! UserManual topics. Nothing here carries independent control semantics.

use crate::descriptor::{ControlKind, ControlSpec, IntegerSpec, NumberSpec};
use crate::registry::{Registry, SectionSpec};
use crate::store::ValueStore;
use crate::value::ControlError;
use serde_json::{Map, Value as Json};

pub const SCHEMA: &str = "hsk.studio.controls@1";

fn actions(spec: &ControlSpec) -> Vec<&'static str> {
    match spec.kind {
        ControlKind::Number(_) | ControlKind::Integer(_) => vec!["set", "step_up", "step_down", "pointer", "reset"],
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
    if let (Json::Object(obj), ControlKind::Integer(_)) = (&mut v, &spec.kind)
        && let Some(Json::Object(kind)) = obj.get_mut("kind")
    {
        // STU-FX-110: integer step = coarse_step = fine_step = 1.
        for key in ["step", "fine_step", "coarse_step"] {
            kind.insert(key.into(), to_json(&1));
        }
    }
    if let Json::Object(obj) = &mut v {
        obj.insert("consumers".into(), to_json(&registry.consumers_of(&spec.id)));
        obj.insert("actions".into(), to_json(&actions(spec)));
        if let Some(value) = store.and_then(|s| s.get(&spec.id)) {
            obj.insert("value".into(), to_json(value));
            obj.insert("is_default".into(), Json::Bool(*value == spec.default));
        }
    }
    v
}

fn section_json(registry: &Registry, section: &SectionSpec) -> Json {
    let mut v = to_json(section);
    if let Json::Object(obj) = &mut v {
        let controls: Vec<&str> = registry.controls_in(Some(&section.id)).iter().map(|c| c.id.as_str()).collect();
        let children: Vec<&str> = registry.children_of(Some(&section.id)).iter().map(|s| s.id.as_str()).collect();
        obj.insert("controls".into(), to_json(&controls));
        obj.insert("child_sections".into(), to_json(&children));
    }
    v
}

/// Deterministic pretty JSON for agents: every section and descriptor with all four bounds, the
/// derived step trio, help text, group path, consumers, allowed actions and (when a store is
/// given) the current value and whether it equals the default.
pub fn agent_json(registry: &Registry, store: Option<&ValueStore>) -> String {
    let sections: Vec<Json> = registry.sections().iter().map(|s| section_json(registry, s)).collect();
    let controls: Vec<Json> = registry.specs().iter().map(|s| control_json(registry, s, store)).collect();
    let mut root = Map::new();
    root.insert("schema".into(), Json::String(SCHEMA.to_string()));
    root.insert("sections".into(), Json::Array(sections));
    root.insert("controls".into(), Json::Array(controls));
    serde_json::to_string_pretty(&Json::Object(root)).unwrap_or_default()
}

fn bad_json(reason: impl Into<String>) -> ControlError {
    ControlError::InvalidDescriptor { control_id: "<controls-json>".to_string(), reason: reason.into() }
}

/// Rebuilds a registry (sections, descriptors and consumers) from [`agent_json`] output. Derived
/// fields (steps, value, actions, section member lists) are ignored; everything is fully
/// re-validated on registration.
pub fn registry_from_json(json: &str) -> Result<Registry, ControlError> {
    let root: Json = serde_json::from_str(json).map_err(|e| bad_json(e.to_string()))?;
    if root.get("schema").and_then(Json::as_str) != Some(SCHEMA) {
        return Err(bad_json(format!("schema must be `{SCHEMA}`")));
    }
    let controls = root.get("controls").and_then(Json::as_array).ok_or_else(|| bad_json("missing `controls` array"))?;
    let mut registry = Registry::new();
    for s in root.get("sections").and_then(Json::as_array).into_iter().flatten() {
        let section: SectionSpec = serde_json::from_value(s.clone()).map_err(|e| bad_json(e.to_string()))?;
        registry.register_section(section)?;
    }
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

/// One UserManual topic derived from a descriptor (topic id = control or section id).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManualTopic {
    pub id: String,
    pub title: String,
    pub body: String,
}

fn bound(b: Option<f64>) -> String {
    b.map_or_else(|| "none".to_string(), |v| v.to_string())
}

fn bound_i(b: Option<i64>) -> String {
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

fn integer_lines(spec: &ControlSpec, n: &IntegerSpec) -> String {
    format!(
        "Whole numbers. Hard range: [{}, {}] (typed values outside are rejected, never clamped).\n\
         Soft range: [{}, {}] (presentation and slider travel; values beyond it are legal and flagged).\n\
         Every step moves exactly one value; modifier keys never skip values.{}",
        bound_i(n.hard_min),
        bound_i(n.hard_max),
        bound_i(n.soft_min),
        bound_i(n.soft_max),
        spec.unit.as_ref().map(|u| format!("\nUnit: {u}.")).unwrap_or_default(),
    )
}

/// Topics for every section (listing its members) followed by every control (in registration
/// order); section path is shown on each control topic.
pub fn manual_topics(registry: &Registry) -> Vec<ManualTopic> {
    let mut topics: Vec<ManualTopic> = registry
        .sections()
        .iter()
        .map(|sec| {
            let members: Vec<String> = registry
                .controls_in(Some(&sec.id))
                .iter()
                .map(|c| format!("{} (`{}`)", c.label, c.id))
                .chain(registry.children_of(Some(&sec.id)).iter().map(|s| format!("section {} (`{}`)", s.label, s.id)))
                .collect();
            ManualTopic {
                id: sec.id.clone(),
                title: sec.label.clone(),
                body: format!(
                    "{}\nContains: {}.\n{}",
                    sec.help,
                    members.join(", "),
                    if sec.collapsed_by_default { "Starts collapsed." } else { "Starts open." }
                ),
            }
        })
        .collect();
    topics.extend(registry.specs().iter().map(|spec| {
        let detail = match &spec.kind {
            ControlKind::Number(n) => number_lines(spec, n),
            ControlKind::Integer(n) => integer_lines(spec, n),
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
        let place = if spec.group_path.is_empty() {
            String::new()
        } else {
            format!("\nSection: {}.", spec.group_path.join(" / "))
        };
        ManualTopic {
            id: spec.id.clone(),
            title: spec.label.clone(),
            body: format!("{}\n{}\nDefault: {}.{}", spec.help, detail, spec.format_value(&spec.default), place),
        }
    }));
    topics
}
