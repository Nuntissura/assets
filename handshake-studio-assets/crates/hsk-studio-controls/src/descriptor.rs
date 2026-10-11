//! Control descriptors: the one configuration that drives the egui renderer, the agent JSON
//! projection and the UserManual topic. Descriptors are also the single owner of value
//! legality: typed text, pointer scrubbing, keyboard steps and model sets all validate here.

use crate::value::{ControlError, Value};
use serde::{Deserialize, Serialize};

pub const MAX_ID_CHARS: usize = 96;
pub const MAX_PRECISION: u8 = 9;
pub const MAX_TEXT_CHARS: usize = 65_536;

/// How a number is presented. Both widgets also offer exact typed entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NumberWidget {
    /// Scrub label + typed field.
    Field,
    /// Slider (needs a complete soft or hard range) + scrub label + typed field.
    Slider,
}

/// Which step increment a keyboard/accessibility step uses. A modifier applies once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepModifier {
    None,
    Fine,
    Coarse,
}

/// Presentation state of a number against its soft range (STU-FX-105).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SoftState {
    InRange,
    OutOfSoft,
}

/// Number configuration. Hard and soft bounds are ALWAYS four separate fields (STU-FX-107).
/// `precision` = declared decimal places (STU-FX-109); `step = 10^-precision`,
/// `fine_step = step / 10`, `coarse_step = step * 10` (STU-FX-110) are derived, never stored.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NumberSpec {
    pub widget: NumberWidget,
    pub precision: u8,
    pub hard_min: Option<f64>,
    pub hard_max: Option<f64>,
    pub soft_min: Option<f64>,
    pub soft_max: Option<f64>,
}

fn pow10_neg(n: i32) -> f64 {
    if n >= 0 { 1.0 / 10f64.powi(n) } else { 10f64.powi(-n) }
}

/// Removes binary-representation noise from a computed step (0.5 + 0.01 -> 0.51).
fn clean(x: f64) -> f64 {
    if x.abs() < 1.0e6 { (x * 1.0e9).round() / 1.0e9 } else { x }
}

impl NumberSpec {
    /// Field-style number with hard bounds only (soft bounds absent).
    pub fn field(precision: u8, hard_min: Option<f64>, hard_max: Option<f64>) -> Self {
        Self { widget: NumberWidget::Field, precision, hard_min, hard_max, soft_min: None, soft_max: None }
    }

    /// Slider-style number; soft bounds govern the track, hard bounds the acceptance range.
    pub fn slider(precision: u8, hard: (f64, f64), soft: (f64, f64)) -> Self {
        Self {
            widget: NumberWidget::Slider,
            precision,
            hard_min: Some(hard.0),
            hard_max: Some(hard.1),
            soft_min: Some(soft.0),
            soft_max: Some(soft.1),
        }
    }

    pub fn step(&self) -> f64 {
        pow10_neg(i32::from(self.precision))
    }
    pub fn fine_step(&self) -> f64 {
        pow10_neg(i32::from(self.precision) + 1)
    }
    pub fn coarse_step(&self) -> f64 {
        pow10_neg(i32::from(self.precision) - 1)
    }

    /// Track range for the slider widget: the complete soft pair, else the complete hard pair.
    pub fn track(&self) -> Option<(f64, f64)> {
        match (self.soft_min, self.soft_max, self.hard_min, self.hard_max) {
            (Some(a), Some(b), _, _) => Some((a, b)),
            (_, _, Some(a), Some(b)) => Some((a, b)),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChoiceOption {
    pub id: String,
    pub label: String,
}

impl ChoiceOption {
    pub fn new(id: &str, label: &str) -> Self {
        Self { id: id.to_string(), label: label.to_string() }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ControlKind {
    Number(NumberSpec),
    Toggle,
    Choice { options: Vec<ChoiceOption> },
    Color,
    Text { max_chars: usize, multiline: bool },
}

/// One control. `id` is stable: it is the egui id source, the store key, the agent address and
/// the manual topic id.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ControlSpec {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub unit: Option<String>,
    /// UserManual text. Required: a control without help text is not registrable.
    pub help: String,
    pub kind: ControlKind,
    pub default: Value,
}

fn invalid(id: &str, reason: impl Into<String>) -> ControlError {
    ControlError::InvalidDescriptor { control_id: id.to_string(), reason: reason.into() }
}

fn no_controls(s: &str) -> bool {
    !s.chars().any(char::is_control)
}

impl ControlSpec {
    pub fn new(id: &str, label: &str, help: &str, kind: ControlKind, default: Value) -> Self {
        Self { id: id.to_string(), label: label.to_string(), unit: None, help: help.to_string(), kind, default }
    }

    pub fn number(id: &str, label: &str, help: &str, spec: NumberSpec, default: f64) -> Self {
        Self::new(id, label, help, ControlKind::Number(spec), Value::Number(default))
    }

    pub fn toggle(id: &str, label: &str, help: &str, default: bool) -> Self {
        Self::new(id, label, help, ControlKind::Toggle, Value::Bool(default))
    }

    pub fn choice(id: &str, label: &str, help: &str, options: Vec<ChoiceOption>, default: &str) -> Self {
        Self::new(id, label, help, ControlKind::Choice { options }, Value::Choice(default.to_string()))
    }

    pub fn color(id: &str, label: &str, help: &str, default: [u8; 4]) -> Self {
        Self::new(id, label, help, ControlKind::Color, Value::Color(default))
    }

    pub fn text(id: &str, label: &str, help: &str, max_chars: usize, multiline: bool, default: &str) -> Self {
        Self::new(id, label, help, ControlKind::Text { max_chars, multiline }, Value::Text(default.to_string()))
    }

    pub fn with_unit(mut self, unit: &str) -> Self {
        self.unit = Some(unit.to_string());
        self
    }

    pub fn number_spec(&self) -> Option<&NumberSpec> {
        match &self.kind {
            ControlKind::Number(n) => Some(n),
            _ => None,
        }
    }

    /// Descriptor legality. Run on registration; a descriptor that passes can always be rendered,
    /// projected and given a manual topic.
    pub fn validate(&self) -> Result<(), ControlError> {
        let id = self.id.as_str();
        let id_ok = !id.is_empty()
            && id.len() <= MAX_ID_CHARS
            && id.starts_with(|c: char| c.is_ascii_lowercase())
            && id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-'));
        if !id_ok {
            return Err(invalid(id, "id must be 1-96 chars of [a-z0-9._-] starting with a letter"));
        }
        if self.label.trim().is_empty() || self.label.chars().count() > 128 || !no_controls(&self.label) {
            return Err(invalid(id, "label must be 1-128 printable chars"));
        }
        if self.help.trim().is_empty() || self.help.chars().count() > 1024 {
            return Err(invalid(id, "help (UserManual text) must be 1-1024 chars"));
        }
        if let Some(u) = &self.unit {
            if !matches!(self.kind, ControlKind::Number(_)) {
                return Err(invalid(id, "unit is only valid on number controls"));
            }
            if u.is_empty() || u.chars().count() > 8 || u.chars().any(|c| c.is_whitespace() || c.is_control()) {
                return Err(invalid(id, "unit must be 1-8 chars without whitespace"));
            }
        }
        match &self.kind {
            ControlKind::Number(n) => Self::validate_number(id, n)?,
            ControlKind::Toggle | ControlKind::Color => {}
            ControlKind::Choice { options } => {
                if options.is_empty() {
                    return Err(invalid(id, "choice needs at least one option"));
                }
                for (i, o) in options.iter().enumerate() {
                    if o.id.is_empty() || o.label.trim().is_empty() || !no_controls(&o.id) || !no_controls(&o.label) {
                        return Err(invalid(id, "choice option id and label must be non-empty printable text"));
                    }
                    if options[..i].iter().any(|p| p.id == o.id) {
                        return Err(invalid(id, format!("duplicate choice option `{}`", o.id)));
                    }
                }
            }
            ControlKind::Text { max_chars, .. } => {
                if *max_chars == 0 || *max_chars > MAX_TEXT_CHARS {
                    return Err(invalid(id, "text max_chars must be 1..=65536"));
                }
            }
        }
        self.validate_value(&self.default).map(|_| ())
    }

    fn validate_number(id: &str, n: &NumberSpec) -> Result<(), ControlError> {
        if n.precision > MAX_PRECISION {
            return Err(invalid(id, "precision must be <= 9"));
        }
        let bounds = [n.hard_min, n.hard_max, n.soft_min, n.soft_max];
        if bounds.iter().flatten().any(|b| !b.is_finite()) {
            return Err(invalid(id, "bounds must be finite"));
        }
        // Order chain hard_min <= soft_min <= soft_max <= hard_max over the bounds that exist.
        let ordered = [n.hard_min, n.soft_min, n.soft_max, n.hard_max].iter().flatten().copied().collect::<Vec<_>>();
        if ordered.windows(2).any(|w| w[0] > w[1]) {
            return Err(invalid(id, "bounds must satisfy hard_min <= soft_min <= soft_max <= hard_max"));
        }
        if n.widget == NumberWidget::Slider && n.track().is_none() {
            return Err(invalid(id, "slider needs a complete soft or hard range"));
        }
        Ok(())
    }

    /// The single legality check. Hard-bound violations are an error naming the control, the
    /// value and both bounds; they are never clamped (STU-FX-104).
    pub fn validate_value(&self, value: &Value) -> Result<Value, ControlError> {
        let id = self.id.clone();
        let mismatch = |expected: &'static str| ControlError::TypeMismatch {
            control_id: id.clone(),
            expected,
            got: value.kind_name(),
        };
        match (&self.kind, value) {
            (ControlKind::Number(n), Value::Number(v)) => {
                if !v.is_finite() {
                    return Err(ControlError::NonFinite { control_id: id });
                }
                let below = n.hard_min.is_some_and(|m| *v < m);
                let above = n.hard_max.is_some_and(|m| *v > m);
                if below || above {
                    return Err(ControlError::OutOfHardBounds {
                        control_id: id,
                        value: *v,
                        hard_min: n.hard_min,
                        hard_max: n.hard_max,
                    });
                }
                Ok(Value::Number(if *v == 0.0 { 0.0 } else { *v }))
            }
            (ControlKind::Number(_), _) => Err(mismatch("number")),
            (ControlKind::Toggle, Value::Bool(_)) => Ok(value.clone()),
            (ControlKind::Toggle, _) => Err(mismatch("bool")),
            (ControlKind::Choice { options }, Value::Choice(c)) => {
                if options.iter().any(|o| &o.id == c) {
                    Ok(value.clone())
                } else {
                    Err(ControlError::UnknownChoice { control_id: id, choice: c.clone() })
                }
            }
            (ControlKind::Choice { .. }, _) => Err(mismatch("choice")),
            (ControlKind::Color, Value::Color(_)) => Ok(value.clone()),
            (ControlKind::Color, _) => Err(mismatch("color")),
            (ControlKind::Text { max_chars, multiline }, Value::Text(t)) => {
                let n = t.chars().count();
                if n > *max_chars {
                    return Err(ControlError::TextTooLong { control_id: id, max_chars: *max_chars, got_chars: n });
                }
                let bad = t.chars().any(|c| c.is_control() && !(*multiline && c == '\n'));
                if bad {
                    return Err(ControlError::InvalidText { control_id: id, reason: "control characters" });
                }
                Ok(value.clone())
            }
            (ControlKind::Text { .. }, _) => Err(mismatch("text")),
        }
    }

    /// Strict typed-entry parser: no arithmetic, no junk stripping. Numbers: optional sign,
    /// digits, optional dot + digits, optional exponent, optional exact unit suffix.
    pub fn parse_text(&self, text: &str) -> Result<Value, ControlError> {
        let id = self.id.clone();
        let bad = |reason: &'static str| ControlError::InvalidText { control_id: id.clone(), reason };
        let value = match &self.kind {
            ControlKind::Number(_) => {
                let mut t = text.trim();
                if let Some(u) = &self.unit
                    && let Some(rest) = t.strip_suffix(u.as_str())
                    && !rest.trim_end().is_empty()
                {
                    t = rest.trim_end();
                }
                let unsigned = t.strip_prefix(['+', '-']).unwrap_or(t).to_ascii_lowercase();
                if matches!(unsigned.as_str(), "nan" | "inf" | "infinity") {
                    return Err(ControlError::NonFinite { control_id: id });
                }
                if !strict_decimal(t) {
                    return Err(bad("not a plain decimal number"));
                }
                let v: f64 = t.parse().map_err(|_| bad("not a plain decimal number"))?;
                if !v.is_finite() {
                    return Err(ControlError::NonFinite { control_id: id });
                }
                Value::Number(v)
            }
            ControlKind::Toggle => match text.trim() {
                "true" | "on" => Value::Bool(true),
                "false" | "off" => Value::Bool(false),
                _ => return Err(bad("expected true/false/on/off")),
            },
            ControlKind::Choice { .. } => Value::Choice(text.trim().to_string()),
            ControlKind::Color => Value::Color(parse_hex_color(text.trim()).ok_or_else(|| bad("expected #rrggbb or #rrggbbaa"))?),
            ControlKind::Text { .. } => Value::Text(text.to_string()),
        };
        self.validate_value(&value)
    }

    /// Display text for a value (numbers use the declared precision; the stored value keeps full
    /// precision, display rounds only).
    pub fn format_value(&self, value: &Value) -> String {
        match (&self.kind, value) {
            (ControlKind::Number(n), Value::Number(v)) => {
                let v = if *v == 0.0 { 0.0 } else { *v };
                format!("{v:.*}", usize::from(n.precision))
            }
            (_, Value::Bool(b)) => b.to_string(),
            (ControlKind::Choice { options }, Value::Choice(c)) => {
                options.iter().find(|o| &o.id == c).map_or_else(|| c.clone(), |o| o.label.clone())
            }
            (_, Value::Color([r, g, b, a])) => format!("#{r:02x}{g:02x}{b:02x}{a:02x}"),
            (_, Value::Text(t)) => t.clone(),
            (_, other) => format!("{other:?}"),
        }
    }

    pub fn soft_state(&self, value: &Value) -> SoftState {
        match (&self.kind, value) {
            (ControlKind::Number(n), Value::Number(v)) => {
                if n.soft_min.is_some_and(|m| *v < m) || n.soft_max.is_some_and(|m| *v > m) {
                    SoftState::OutOfSoft
                } else {
                    SoftState::InRange
                }
            }
            _ => SoftState::InRange,
        }
    }

    /// Pointer route: the ONLY route that clamps, and only to the hard bounds; it crosses soft
    /// bounds (STU-FX-110a).
    pub fn pointer_value(&self, target: f64) -> Result<Value, ControlError> {
        let ControlKind::Number(n) = &self.kind else {
            return Err(ControlError::TypeMismatch { control_id: self.id.clone(), expected: "number", got: "pointer" });
        };
        if !target.is_finite() {
            return Err(ControlError::NonFinite { control_id: self.id.clone() });
        }
        let mut v = target;
        if let Some(m) = n.hard_min {
            v = v.max(m);
        }
        if let Some(m) = n.hard_max {
            v = v.min(m);
        }
        self.validate_value(&Value::Number(v))
    }

    /// Keyboard/accessibility step: one increment (modifier applied once). At a hard bound the
    /// step is REJECTED with `OutOfHardBounds` (it never clamps or silently no-ops).
    pub fn step_value(&self, current: &Value, dir: i8, modifier: StepModifier) -> Result<Value, ControlError> {
        let (ControlKind::Number(n), Value::Number(cur)) = (&self.kind, current) else {
            return Err(ControlError::TypeMismatch { control_id: self.id.clone(), expected: "number", got: current.kind_name() });
        };
        let amount = match modifier {
            StepModifier::None => n.step(),
            StepModifier::Fine => n.fine_step(),
            StepModifier::Coarse => n.coarse_step(),
        };
        let next = clean(cur + f64::from(dir.signum()) * amount);
        self.validate_value(&Value::Number(next))
    }
}

fn strict_decimal(s: &str) -> bool {
    let b = s.as_bytes();
    let mut i = 0;
    if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
        i += 1;
    }
    let digits = |i: &mut usize| {
        let start = *i;
        while *i < b.len() && b[*i].is_ascii_digit() {
            *i += 1;
        }
        *i - start
    };
    let mut mantissa = digits(&mut i);
    if i < b.len() && b[i] == b'.' {
        i += 1;
        mantissa += digits(&mut i);
    }
    if mantissa == 0 {
        return false;
    }
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        i += 1;
        if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
            i += 1;
        }
        if digits(&mut i) == 0 {
            return false;
        }
    }
    i == b.len()
}

fn parse_hex_color(s: &str) -> Option<[u8; 4]> {
    let h = s.strip_prefix('#')?;
    if !(h.len() == 6 || h.len() == 8) || !h.bytes().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).ok();
    Some([byte(0)?, byte(2)?, byte(4)?, if h.len() == 8 { byte(6)? } else { 255 }])
}
