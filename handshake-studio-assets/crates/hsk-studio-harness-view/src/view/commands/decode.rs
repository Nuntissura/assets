//! Strict readers of command parameters: JSON values, decoded by hand (this crate has no serde derive;
//! Folio records go through their own `Deserialize`), with typed refusals. A scalar, list or object
//! may also arrive as text, because `--param` and a session line decode alike.
#![deny(clippy::arithmetic_side_effects)]

use super::super::vector::style::{GradientStop, PaintSpec, StrokePanel};
use super::CommandRefusal;
use hsk_studio_folio as folio;
use hsk_studio_pigment::brush::StrokeSample;
use serde_json::{Map, Value, json};
use std::path::PathBuf;

pub(super) fn invalid(key: &str, expected: &str) -> CommandRefusal {
    CommandRefusal::new("parameter_invalid", json!({"key": key, "expected": expected}))
}

pub(super) fn missing(key: &str) -> CommandRefusal {
    CommandRefusal::new("parameter_missing", json!({"key": key}))
}

fn finite(value: &Value) -> Option<f64> {
    value.as_f64().filter(|n| n.is_finite())
}

fn pair(value: &Value) -> Option<[f64; 2]> {
    match value.as_array()?.as_slice() {
        [x, y] => Some([finite(x)?, finite(y)?]),
        _ => None,
    }
}

fn rgb8(value: &Value) -> Option<[u8; 3]> {
    match value.as_array()?.as_slice() {
        [r, g, b] => Some([u8::try_from(r.as_u64()?).ok()?, u8::try_from(g.as_u64()?).ok()?, u8::try_from(b.as_u64()?).ok()?]),
        _ => None,
    }
}

/// One object's fields: the parameters of a command, or a nested record.
#[derive(Clone, Copy)]
pub(super) struct Fields<'a>(pub(super) &'a Map<String, Value>);

impl<'a> Fields<'a> {
    /// A nested record: every field it holds must be one of `known`.
    pub(super) fn record(value: &'a Value, key: &str, known: &[&str]) -> Result<Self, CommandRefusal> {
        let Value::Object(map) = value else {
            return Err(invalid(key, "a JSON object"));
        };
        if let Some(field) = map.keys().find(|k| !known.contains(&k.as_str())) {
            return Err(CommandRefusal::new("parameter_unknown", json!({"key": format!("{key}.{field}")})));
        }
        Ok(Self(map))
    }

    pub(super) fn has(&self, key: &str) -> bool {
        self.0.get(key).is_some_and(|v| !v.is_null())
    }

    fn value(&self, key: &str) -> Result<&'a Value, CommandRefusal> {
        self.0.get(key).filter(|v| !v.is_null()).ok_or_else(|| missing(key))
    }

    pub(super) fn text(&self, key: &str) -> Result<String, CommandRefusal> {
        match self.value(key)? {
            Value::String(s) => Ok(s.clone()),
            Value::Number(n) => Ok(n.to_string()),
            Value::Bool(b) => Ok(b.to_string()),
            _ => Err(invalid(key, "text")),
        }
    }

    pub(super) fn opt_text(&self, key: &str) -> Result<Option<String>, CommandRefusal> {
        if self.has(key) { self.text(key).map(Some) } else { Ok(None) }
    }

    pub(super) fn number(&self, key: &str) -> Result<f64, CommandRefusal> {
        let value = self.value(key)?;
        let parsed = match value {
            Value::String(s) => s.parse::<f64>().ok(),
            other => other.as_f64(),
        };
        parsed.filter(|n| n.is_finite()).ok_or_else(|| invalid(key, "finite number"))
    }

    pub(super) fn opt_number(&self, key: &str) -> Result<Option<f64>, CommandRefusal> {
        if self.has(key) { self.number(key).map(Some) } else { Ok(None) }
    }

    pub(super) fn number_or(&self, key: &str, default: f64) -> Result<f64, CommandRefusal> {
        Ok(self.opt_number(key)?.unwrap_or(default))
    }

    pub(super) fn flag(&self, key: &str) -> Result<bool, CommandRefusal> {
        match self.value(key)? {
            Value::Bool(b) => Ok(*b),
            Value::String(s) if s == "true" => Ok(true),
            Value::String(s) if s == "false" => Ok(false),
            _ => Err(invalid(key, "boolean")),
        }
    }

    pub(super) fn flag_or(&self, key: &str, default: bool) -> Result<bool, CommandRefusal> {
        if self.has(key) { self.flag(key) } else { Ok(default) }
    }

    pub(super) fn unsigned(&self, key: &str) -> Result<u64, CommandRefusal> {
        let value = self.value(key)?;
        let parsed = match value {
            Value::String(s) => s.parse::<u64>().ok(),
            other => other.as_u64(),
        };
        parsed.ok_or_else(|| invalid(key, "unsigned integer"))
    }

    pub(super) fn count(&self, key: &str) -> Result<u32, CommandRefusal> {
        u32::try_from(self.unsigned(key)?).map_err(|_| invalid(key, "integer in 0..=4294967295"))
    }

    pub(super) fn index(&self, key: &str) -> Result<usize, CommandRefusal> {
        usize::try_from(self.unsigned(key)?).map_err(|_| invalid(key, "index"))
    }

    pub(super) fn path(&self, key: &str) -> Result<PathBuf, CommandRefusal> {
        self.text(key).map(PathBuf::from)
    }

    /// A list or object, given as JSON or as its JSON text.
    pub(super) fn json(&self, key: &str, expected: &str) -> Result<Value, CommandRefusal> {
        match self.value(key)? {
            Value::String(s) => serde_json::from_str(s).map_err(|_| invalid(key, expected)),
            other => Ok(other.clone()),
        }
    }

    /// Text items: a JSON list of strings, its JSON text, or comma-separated text.
    pub(super) fn list(&self, key: &str) -> Result<Vec<String>, CommandRefusal> {
        let expected = "a list of text";
        match self.value(key)? {
            Value::String(s) if s.trim_start().starts_with('[') => {
                let items: Value = serde_json::from_str(s).map_err(|_| invalid(key, expected))?;
                strings(&items).ok_or_else(|| invalid(key, expected))
            }
            Value::String(s) => Ok(s.split(',').filter(|p| !p.is_empty()).map(str::to_string).collect()),
            other => strings(other).ok_or_else(|| invalid(key, expected)),
        }
    }

    pub(super) fn point(&self, key: &str) -> Result<[f64; 2], CommandRefusal> {
        pair(&self.json(key, "[x, y]")?).ok_or_else(|| invalid(key, "[x, y]"))
    }

    pub(super) fn opt_point(&self, key: &str) -> Result<Option<[f64; 2]>, CommandRefusal> {
        if self.has(key) { self.point(key).map(Some) } else { Ok(None) }
    }

    pub(super) fn points(&self, key: &str) -> Result<Vec<[f64; 2]>, CommandRefusal> {
        let value = self.json(key, "[[x, y], ...]")?;
        value.as_array().and_then(|items| items.iter().map(pair).collect()).ok_or_else(|| invalid(key, "[[x, y], ...]"))
    }

    pub(super) fn numbers(&self, key: &str) -> Result<Vec<f64>, CommandRefusal> {
        let value = self.json(key, "[n, ...]")?;
        value.as_array().and_then(|items| items.iter().map(finite).collect()).ok_or_else(|| invalid(key, "[n, ...]"))
    }

    /// Exactly `N` finite numbers.
    pub(super) fn vector<const N: usize>(&self, key: &str) -> Result<[f64; N], CommandRefusal> {
        let expected = "a list of finite numbers of the declared length";
        let items = self.numbers(key).map_err(|_| invalid(key, expected))?;
        <[f64; N]>::try_from(items).map_err(|_| invalid(key, expected))
    }

    pub(super) fn rgb8(&self, key: &str) -> Result<[u8; 3], CommandRefusal> {
        rgb8(&self.json(key, "[r, g, b]")?).ok_or_else(|| invalid(key, "[r, g, b], each 0..=255"))
    }

    pub(super) fn opt_rgb8(&self, key: &str) -> Result<Option<[u8; 3]>, CommandRefusal> {
        if self.has(key) { self.rgb8(key).map(Some) } else { Ok(None) }
    }

    /// `[[x, y], ...]` or `[[x, y, pressure], ...]`.
    pub(super) fn samples(&self, key: &str) -> Result<Vec<StrokeSample>, CommandRefusal> {
        let expected = "[[x, y] or [x, y, pressure], ...]";
        let value = self.json(key, expected)?;
        let sample = |v: &Value| match v.as_array()?.as_slice() {
            [x, y] => Some(StrokeSample { x: finite(x)?, y: finite(y)?, pressure: None, time_ms: None }),
            #[allow(clippy::cast_possible_truncation)]
            [x, y, p] => Some(StrokeSample { x: finite(x)?, y: finite(y)?, pressure: Some(finite(p)? as f32), time_ms: None }),
            _ => None,
        };
        value.as_array().and_then(|items| items.iter().map(sample).collect()).ok_or_else(|| invalid(key, expected))
    }

    /// The member of `table` the text names; the refusal lists the tokens.
    pub(super) fn token<T: Copy>(&self, key: &str, table: &[(&str, T)]) -> Result<T, CommandRefusal> {
        let given = self.text(key)?;
        table.iter().find(|(token, _)| *token == given).map(|(_, member)| *member).ok_or_else(|| {
            let tokens: Vec<&str> = table.iter().map(|(token, _)| *token).collect();
            invalid(key, &tokens.join("|"))
        })
    }

    /// A nested record held under `key` (JSON or its JSON text), with its allowed fields.
    pub(super) fn nested<R>(&self, key: &str, known: &[&str], read: impl FnOnce(Fields<'_>) -> Result<R, CommandRefusal>) -> Result<R, CommandRefusal> {
        let value = self.json(key, "a JSON object")?;
        read(Fields::record(&value, key, known)?)
    }

    /// The raw value of a Folio record or token; [`folio_field`] decodes it.
    pub(super) fn raw(&self, key: &str, expected: &str) -> Result<Value, CommandRefusal> {
        match self.value(key)? {
            Value::String(s) if s.trim_start().starts_with(['{', '[']) => serde_json::from_str(s).map_err(|_| invalid(key, expected)),
            other => Ok(other.clone()),
        }
    }
}

/// A Folio record or token under a key, through Folio's own `Deserialize`. A macro, because this crate
/// has no serde edge to name the bound of a generic function with.
macro_rules! folio_field {
    ($fields:expr, $key:expr, $ty:ty, $expected:expr) => {
        serde_json::from_value::<$ty>($fields.raw($key, $expected)?).map_err(|_| super::decode::invalid($key, $expected))
    };
}
pub(super) use folio_field;

fn strings(value: &Value) -> Option<Vec<String>> {
    value.as_array()?.iter().map(|v| v.as_str().map(str::to_string)).collect()
}

/// `{"kind":"solid","rgb8":[r,g,b]}`, `{"kind":"linear","from":[x,y],"to":[x,y],"stops":[...]}` or
/// `{"kind":"radial","centre":[x,y],"radius":r,"stops":[...]}`; a stop is
/// `{"position":0..1,"rgb8":[r,g,b],"opacity":0..1}`, opacity 1 by default.
pub(super) fn paint(value: &Value, key: &str) -> Result<PaintSpec, CommandRefusal> {
    let f = Fields::record(value, key, &["kind", "rgb8", "from", "to", "centre", "radius", "stops"])?;
    let stops = || -> Result<Vec<GradientStop>, CommandRefusal> {
        let list = f.json("stops", "a list of stops")?;
        let Some(items) = list.as_array() else { return Err(invalid("stops", "a list of stops")) };
        items
            .iter()
            .map(|item| {
                let s = Fields::record(item, "stops", &["position", "rgb8", "opacity"])?;
                Ok(GradientStop { position: s.number("position")?, colour: s.rgb8("rgb8")?, opacity: s.number_or("opacity", 1.0)? })
            })
            .collect()
    };
    Ok(match f.text("kind")?.as_str() {
        "solid" => PaintSpec::Solid(f.rgb8("rgb8")?),
        "linear" => PaintSpec::Linear { from: f.point("from")?, to: f.point("to")?, stops: stops()? },
        "radial" => PaintSpec::Radial { centre: f.point("centre")?, radius: f.number("radius")?, stops: stops()? },
        _ => return Err(invalid("kind", "solid|linear|radial")),
    })
}

/// The stroke panel: `paint` and `weight`; the rest default as a solid stroke does.
pub(super) fn stroke_panel(value: &Value, key: &str) -> Result<StrokePanel, CommandRefusal> {
    let f = Fields::record(value, key, &["paint", "weight", "cap", "join", "miter_limit", "dash", "dash_offset", "align"])?;
    let base = StrokePanel::solid([0, 0, 0], 1.0);
    let paint_value = f.json("paint", "a paint")?;
    Ok(StrokePanel {
        paint: paint(&paint_value, "paint")?,
        weight: f.number("weight")?,
        cap: if f.has("cap") { serde_json::from_value::<folio::StrokeCap>(f.raw("cap", "butt|round|projecting")?).map_err(|_| invalid("cap", "butt|round|projecting"))? } else { base.cap },
        join: if f.has("join") { serde_json::from_value::<folio::StrokeJoin>(f.raw("join", "miter|round|bevel")?).map_err(|_| invalid("join", "miter|round|bevel"))? } else { base.join },
        miter_limit: f.number_or("miter_limit", base.miter_limit)?,
        dash: if f.has("dash") { f.numbers("dash")? } else { base.dash },
        dash_offset: f.number_or("dash_offset", base.dash_offset)?,
        align: if f.has("align") { serde_json::from_value::<folio::StrokeAlign>(f.raw("align", "CENTER|INSIDE|OUTSIDE")?).map_err(|_| invalid("align", "CENTER|INSIDE|OUTSIDE"))? } else { base.align },
    })
}
