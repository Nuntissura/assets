//! Shared leaf records of the design-record contract (`records.rs`): the validation context, the
//! publishable-entity fields, RGB and RGBA colour values, the easing record and the variable value
//! record. Folio stores and validates only; resolving a value, evaluating an easing and mode
//! selection belong to the components and motion crates.
//!
//! Declared v02.215 gaps, not invented here: `EXPRESSION` storage values and the fifteen expression
//! functions of STU-DS-131 are not modelled, so a `CONDITIONAL` reaction action is not modelled
//! either; the easing record carries no `type` member because STU-PRO-119 does not enumerate its
//! tokens (the exactly-one-function rule is what the spec states); colour channels are `0..=1`
//! (extended-range and HDR channel values are not stored); a literal value record omits the
//! redundant `resolved_type` member of STU-DS-127 (it equals `type`; an alias record carries it).
use super::{
    Budget, Code, ProfileBinding, Resolution, ResolutionIssue, Resolver, Result, canceled, err, id,
    required, timeline::control_free,
};
use hsk_studio_accord::CancellationToken;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Longest free text member (description, string variable value).
pub(crate) const MAX_TEXT_BYTES: usize = 65_536;
/// Longest documentation link.
const MAX_URL_BYTES: usize = 2048;
/// Documentation links per publishable entity (a Folio hard ceiling; the spec declares none).
const MAX_LINKS: usize = 64;

/// Validation context shared by every record validator: cancellation, the caller resolver, the
/// remaining node and text budget and the unresolved references found so far.
pub(crate) struct Ctx<'a> {
    pub(crate) token: &'a CancellationToken,
    pub(crate) resolver: &'a dyn Resolver,
    pub(crate) issues: Vec<ResolutionIssue>,
    nodes: usize,
    bytes: usize,
    profiles: BTreeSet<String>,
}
impl<'a> Ctx<'a> {
    pub(crate) fn new(b: Budget, token: &'a CancellationToken, resolver: &'a dyn Resolver) -> Self {
        Self {
            token,
            resolver,
            issues: Vec::new(),
            nodes: b.nodes,
            bytes: b.payload_bytes,
            profiles: BTreeSet::new(),
        }
    }
    pub(crate) fn tick(&self) -> Result<()> {
        canceled(self.token)
    }
    /// Charges `n` records against `Budget::nodes`.
    pub(crate) fn nodes(&mut self, n: usize, target: &str) -> Result<()> {
        self.nodes = self
            .nodes
            .checked_sub(n)
            .ok_or_else(|| err(Code::BudgetExceeded, target))?;
        Ok(())
    }
    /// Charges the text against `Budget::payload_bytes`.
    pub(crate) fn text(&mut self, s: &str, target: &str) -> Result<()> {
        self.bytes = self
            .bytes
            .checked_sub(s.len())
            .ok_or_else(|| err(Code::BudgetExceeded, target))?;
        Ok(())
    }
    /// An explicit colour-profile binding: `SCPF` id, resolved once per distinct id.
    pub(crate) fn profile(&mut self, p: &ProfileBinding, target: &str) -> Result<()> {
        let ProfileBinding::Assigned(v) = p else {
            return Ok(());
        };
        id(v, "SCPF", target)?;
        if self.profiles.insert(v.clone()) {
            let disposition = self.resolver.profile(v);
            if disposition != Resolution::Available {
                self.issues.push(ResolutionIssue {
                    target: format!("{target}/colour_profile"),
                    disposition,
                });
            }
        }
        Ok(())
    }
}

pub(crate) fn percent(v: f64) -> bool {
    v.is_finite() && (0.0..=100.0).contains(&v)
}
pub(crate) fn unit_interval(v: f64) -> bool {
    v.is_finite() && (0.0..=1.0).contains(&v)
}
/// A short identifying label (map key, property name, mode id): non-empty, at most 128 bytes, no
/// control characters.
pub(crate) fn label(s: &str) -> bool {
    !s.is_empty() && s.len() <= 128 && !s.chars().any(char::is_control)
}
/// A slash-path name (STU-DS-005a organisation): no empty segment.
pub(crate) fn slash_path(s: &str) -> bool {
    control_free(s) && s.split('/').all(|segment| !segment.trim().is_empty())
}

/// STU-DS-141 documentation link.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioDocumentationLink {
    pub name: String,
    pub url: String,
}

/// STU-DS-141 publishable-entity common fields: one contract for components, styles, variables and
/// collections, so it is one record shared by all of them (the spec lists the five members flat).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioPublishing {
    pub description: String,
    pub description_markdown: String,
    pub documentation_links: Vec<StudioDocumentationLink>,
    /// Stable cross-document key; distinct from the entity id.
    pub publish_key: String,
    /// True when the definition arrived from a subscribed library.
    pub is_remote: bool,
}
impl StudioPublishing {
    pub(crate) fn validate(&self, owner: &str, c: &mut Ctx) -> Result<()> {
        c.tick()?;
        let target = format!("{owner}/publishing");
        for text in [&self.description, &self.description_markdown] {
            if text.len() > MAX_TEXT_BYTES {
                return Err(err(Code::InvalidField, target));
            }
            c.text(text, &target)?;
        }
        if !control_free(&self.publish_key) || self.documentation_links.len() > MAX_LINKS {
            return Err(err(Code::InvalidField, target));
        }
        c.text(&self.publish_key, &target)?;
        for link in &self.documentation_links {
            if !control_free(&link.name)
                || link.url.is_empty()
                || link.url.len() > MAX_URL_BYTES
                || link.url.chars().any(char::is_control)
            {
                return Err(err(Code::InvalidField, target));
            }
            c.text(&link.name, &target)?;
            c.text(&link.url, &target)?;
        }
        Ok(())
    }
}

/// Opaque RGB colour with an explicit colour-profile binding (`SCPF` id, or explicit null for the
/// document working space). Channels are `0..=1`. Opacity is not part of the colour: it lives on the
/// gradient stop and on the appearance row (STU-VEC-015, STU-VEC-136).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioColor {
    #[schemars(range(min = 0.0, max = 1.0))]
    pub r: f64,
    #[schemars(range(min = 0.0, max = 1.0))]
    pub g: f64,
    #[schemars(range(min = 0.0, max = 1.0))]
    pub b: f64,
    #[serde(deserialize_with = "required")]
    pub colour_profile_id: ProfileBinding,
}
impl StudioColor {
    pub(crate) fn validate(&self, target: &str, c: &mut Ctx) -> Result<()> {
        if ![self.r, self.g, self.b].into_iter().all(unit_interval) {
            return Err(err(Code::InvalidField, format!("{target}/colour")));
        }
        c.profile(&self.colour_profile_id, target)
    }
}

/// RGBA colour value (variable `COLOR` values, STU-DS-127): `a` is `0..=1` and carries opacity.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioRgba {
    #[schemars(range(min = 0.0, max = 1.0))]
    pub r: f64,
    #[schemars(range(min = 0.0, max = 1.0))]
    pub g: f64,
    #[schemars(range(min = 0.0, max = 1.0))]
    pub b: f64,
    #[schemars(range(min = 0.0, max = 1.0))]
    pub a: f64,
    #[serde(deserialize_with = "required")]
    pub colour_profile_id: ProfileBinding,
}
impl StudioRgba {
    fn validate(&self, target: &str, c: &mut Ctx) -> Result<()> {
        if ![self.r, self.g, self.b, self.a]
            .into_iter()
            .all(unit_interval)
        {
            return Err(err(Code::InvalidField, format!("{target}/colour")));
        }
        c.profile(&self.colour_profile_id, target)
    }
}

/// STU-PRO-119 cubic-bezier easing: `x1`/`x2` in `0..=1`, `y1`/`y2` in `-10..=10` (Studio-declared).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CubicBezier {
    pub x1: f64,
    pub y1: f64,
    pub x2: f64,
    pub y2: f64,
}

/// STU-PRO-119 spring easing; the bounds are the Studio-declared hard bounds of the spec.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SpringEasing {
    /// `(0, 100]`.
    pub mass: f64,
    /// `(0, 10000]`.
    pub stiffness: f64,
    /// `[0, 1000]`.
    pub damping: f64,
    /// `[-1000, 1000]`.
    pub initial_velocity: f64,
}

/// STU-PRO-119 easing: exactly one of the two function records is present.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioEasing {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub easing_function_cubic_bezier: Option<CubicBezier>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub easing_function_spring: Option<SpringEasing>,
}
impl StudioEasing {
    pub(crate) fn validate(&self, target: &str) -> Result<()> {
        let target = format!("{target}/easing");
        let ok = match (&self.easing_function_cubic_bezier, &self.easing_function_spring) {
            (Some(b), None) => {
                [b.x1, b.x2].into_iter().all(unit_interval)
                    && [b.y1, b.y2]
                        .into_iter()
                        .all(|v| v.is_finite() && (-10.0..=10.0).contains(&v))
            }
            (None, Some(s)) => {
                s.mass.is_finite()
                    && s.mass > 0.0
                    && s.mass <= 100.0
                    && s.stiffness.is_finite()
                    && s.stiffness > 0.0
                    && s.stiffness <= 10_000.0
                    && s.damping.is_finite()
                    && (0.0..=1000.0).contains(&s.damping)
                    && s.initial_velocity.is_finite()
                    && (-1000.0..=1000.0).contains(&s.initial_velocity)
            }
            _ => false,
        };
        if ok {
            Ok(())
        } else {
            Err(err(Code::InvalidField, target))
        }
    }
}

/// STU-DS-125 resolved variable type enumeration.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum VariableType {
    Boolean,
    Color,
    Easing,
    Float,
    String,
    Timing,
}

/// STU-DS-127 variable value record (see the module note for the omitted `EXPRESSION` form).
/// `TIMING` is milliseconds and non-negative.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "SCREAMING_SNAKE_CASE", deny_unknown_fields)]
pub enum StudioValue {
    Boolean {
        value: bool,
    },
    Color {
        value: StudioRgba,
    },
    Easing {
        value: StudioEasing,
    },
    Float {
        value: f64,
    },
    String {
        value: String,
    },
    Timing {
        value: f64,
    },
    /// `{type: "VARIABLE_ALIAS", id}`: another `SVAR` variable, read at its own mode; the alias
    /// carries the type it must resolve to.
    VariableAlias {
        id: String,
        resolved_type: VariableType,
    },
}
impl StudioValue {
    /// The type this value resolves to (an alias states it).
    pub fn resolved_type(&self) -> VariableType {
        match self {
            Self::Boolean { .. } => VariableType::Boolean,
            Self::Color { .. } => VariableType::Color,
            Self::Easing { .. } => VariableType::Easing,
            Self::Float { .. } => VariableType::Float,
            Self::String { .. } => VariableType::String,
            Self::Timing { .. } => VariableType::Timing,
            Self::VariableAlias { resolved_type, .. } => *resolved_type,
        }
    }
    /// The aliased variable id, when this value is an alias.
    pub fn alias_target(&self) -> Option<&str> {
        match self {
            Self::VariableAlias { id, .. } => Some(id),
            _ => None,
        }
    }
    pub(crate) fn validate(&self, target: &str, c: &mut Ctx) -> Result<()> {
        c.tick()?;
        match self {
            Self::Boolean { .. } => Ok(()),
            Self::Color { value } => value.validate(target, c),
            Self::Easing { value } => value.validate(target),
            Self::Float { value } if value.is_finite() => Ok(()),
            Self::Timing { value } if value.is_finite() && *value >= 0.0 => Ok(()),
            Self::String { value } if value.len() <= MAX_TEXT_BYTES => c.text(value, target),
            Self::VariableAlias { id: alias, .. } => id(alias, "SVAR", target),
            _ => Err(err(Code::InvalidField, format!("{target}/value"))),
        }
    }
}
