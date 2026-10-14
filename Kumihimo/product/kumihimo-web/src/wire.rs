//! Bounded, strict local browser routes. Identifiers never cross JSON as numbers.
use crate::WebError;
use kumihimo_core as core;
use serde::{
    de::{self, MapAccess, SeqAccess, Visitor},
    Deserialize, Deserializer, Serialize, Serializer,
};
use serde_json::Value;
use std::{collections::BTreeSet, fmt};

pub const VERSION: u32 = 1;
pub const MAX_ROUTE_BYTES: usize = 65_536;
const MAX_FIELDS_BYTES: usize = 8192;
const MAX_LIST: usize = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Decimal(pub u64);
impl Serialize for Decimal {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0.to_string())
    }
}
impl<'de> Deserialize<'de> for Decimal {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let text = String::deserialize(d)?;
        if text.is_empty()
            || text.len() > 20
            || !text.bytes().all(|b| b.is_ascii_digit())
            || (text.len() > 1 && text.starts_with('0'))
        {
            return Err(de::Error::custom("noncanonical decimal"));
        }
        text.parse()
            .map(Self)
            .map_err(|_| de::Error::custom("decimal overflow"))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RectWire {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}
impl RectWire {
    pub fn to_rect(self) -> Result<core::Rect, WebError> {
        if self.width <= 0.0 || self.height <= 0.0 {
            return Err(WebError::new("invalid_geometry"));
        }
        core::Rect::new(self.x, self.y, self.width, self.height)
            .map_err(|_| WebError::new("invalid_geometry"))
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Location {
    Append {
        workspace: Decimal,
    },
    WorkspaceEdge {
        workspace: Decimal,
        edge: core::Edge,
    },
    Beside {
        frame: Decimal,
        edge: core::Edge,
    },
    Cell {
        workspace: Decimal,
        grid: Decimal,
        row: usize,
        column: usize,
    },
}
impl Location {
    pub fn to_location(&self) -> Result<core::DockLocation, WebError> {
        Ok(match self {
            Self::Append { workspace } => core::DockLocation::Append {
                workspace: core::WorkspaceId::from_raw(entity(*workspace)?),
            },
            Self::WorkspaceEdge { workspace, edge } => core::DockLocation::WorkspaceEdge {
                workspace: core::WorkspaceId::from_raw(entity(*workspace)?),
                edge: *edge,
            },
            Self::Beside { frame, edge } => core::DockLocation::Beside {
                frame: core::FrameId::from_raw(entity(*frame)?),
                edge: *edge,
            },
            Self::Cell {
                workspace,
                grid,
                row,
                column,
            } => core::DockLocation::Cell {
                workspace: core::WorkspaceId::from_raw(entity(*workspace)?),
                grid: core::NodeId(entity(*grid)?),
                row: *row,
                column: *column,
            },
        })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Action {
    Select {
        view: Decimal,
    },
    MoveFrame {
        frame: Decimal,
        destination: Decimal,
        index: usize,
    },
    MoveView {
        view: Decimal,
        destination: Decimal,
        index: usize,
    },
    Dock {
        frame: Decimal,
        location: Location,
    },
    Float {
        frame: Decimal,
        rect: RectWire,
    },
    Redock {
        frame: Decimal,
    },
    SetLayout {
        workspace: Decimal,
        plan: core::LayoutPlan,
    },
    ResizeSplit {
        workspace: Decimal,
        node: Decimal,
        weights: Vec<f64>,
    },
    GridTracks {
        workspace: Decimal,
        node: Decimal,
        rows: Vec<f64>,
        columns: Vec<f64>,
    },
    Equalize {
        workspace: Decimal,
    },
    Swap {
        first: Decimal,
        second: Decimal,
    },
    Maximize {
        frame: Decimal,
    },
    RestoreLayout {
        workspace: Decimal,
    },
    Expand {
        frame: Decimal,
        expanded: bool,
    },
    Viewport {
        rect: RectWire,
        scale: f64,
    },
    Close {
        view: Decimal,
        approved: bool,
    },
    ProviderAvailability {
        provider: Decimal,
        available: bool,
    },
    CommitPreview {
        token: Decimal,
    },
}
impl Action {
    pub fn dock_intent(&self) -> Result<core::DockIntent, WebError> {
        match self {
            Self::Dock { frame, location } => Ok(core::DockIntent::Frame {
                frame: core::FrameId::from_raw(entity(*frame)?),
                location: location.to_location()?,
            }),
            Self::Float { frame, rect } => Ok(core::DockIntent::Float {
                frame: core::FrameId::from_raw(entity(*frame)?),
                rect: rect.to_rect()?,
            }),
            Self::Redock { frame } => Ok(core::DockIntent::Redock {
                frame: core::FrameId::from_raw(entity(*frame)?),
            }),
            _ => Err(WebError::new("preview_requires_dock")),
        }
    }
    pub fn to_command(&self, snapshot: &core::Snapshot) -> Result<core::Command, WebError> {
        use core::{Command as C, FrameId as F, ViewId as V, WorkspaceId as W};
        self.validate()?;
        Ok(match self {
            Self::Select { view } => C::SelectView {
                view: V::from_raw(entity(*view)?),
            },
            Self::MoveFrame {
                frame,
                destination,
                index,
            } => C::MoveFrame {
                frame: F::from_raw(entity(*frame)?),
                destination: W::from_raw(entity(*destination)?),
                index: *index,
            },
            Self::MoveView {
                view,
                destination,
                index,
            } => C::MoveView {
                view: V::from_raw(entity(*view)?),
                destination: F::from_raw(entity(*destination)?),
                index: *index,
            },
            Self::Dock { .. } | Self::Float { .. } | Self::Redock { .. } => {
                C::Dock(self.dock_intent()?)
            }
            Self::SetLayout { workspace, plan } => C::SetLayout {
                workspace: W::from_raw(entity(*workspace)?),
                plan: plan.clone(),
            },
            Self::ResizeSplit {
                workspace,
                node,
                weights,
            } => {
                let workspace = W::from_raw(entity(*workspace)?);
                let mut plan = snapshot
                    .layouts
                    .get(&workspace)
                    .ok_or_else(|| WebError::new("missing_layout"))?
                    .clone();
                let Some(core::LayoutNode::Split { children, .. }) =
                    plan.nodes.get_mut(&core::NodeId(entity(*node)?))
                else {
                    return Err(WebError::new("missing_split"));
                };
                if children.len() != weights.len() {
                    return Err(WebError::new("track_count_mismatch"));
                }
                for (child, weight) in children.iter_mut().zip(weights) {
                    child.weight = scalar(*weight)?;
                }
                C::SetLayout { workspace, plan }
            }
            Self::GridTracks {
                workspace,
                node,
                rows,
                columns,
            } => {
                let workspace = W::from_raw(entity(*workspace)?);
                let mut plan = snapshot
                    .layouts
                    .get(&workspace)
                    .ok_or_else(|| WebError::new("missing_layout"))?
                    .clone();
                let Some(core::LayoutNode::Grid {
                    rows: r,
                    columns: c,
                    ..
                }) = plan.nodes.get_mut(&core::NodeId(entity(*node)?))
                else {
                    return Err(WebError::new("missing_grid"));
                };
                if rows.len() != r.len() || columns.len() != c.len() {
                    return Err(WebError::new("track_count_mismatch"));
                }
                *r = rows.iter().map(|v| scalar(*v)).collect::<Result<_, _>>()?;
                *c = columns
                    .iter()
                    .map(|v| scalar(*v))
                    .collect::<Result<_, _>>()?;
                C::SetLayout { workspace, plan }
            }
            Self::Equalize { workspace } => C::Equalize {
                workspace: W::from_raw(entity(*workspace)?),
            },
            Self::Swap { first, second } => C::SwapFrames {
                first: F::from_raw(entity(*first)?),
                second: F::from_raw(entity(*second)?),
            },
            Self::Maximize { frame } => C::Maximize {
                frame: F::from_raw(entity(*frame)?),
            },
            Self::RestoreLayout { workspace } => C::RestoreLayout {
                workspace: W::from_raw(entity(*workspace)?),
            },
            Self::Expand { frame, expanded } => {
                let record = snapshot
                    .frames
                    .get(&F::from_raw(entity(*frame)?))
                    .ok_or_else(|| WebError::new("missing_frame"))?;
                let view = record
                    .selected_view
                    .ok_or_else(|| WebError::new("missing_selected_view"))?;
                C::SetViewState {
                    view,
                    state: core::ViewState {
                        visible: *expanded,
                        usable: true,
                    },
                }
            }
            Self::Close { view, approved } => C::CloseView {
                view: V::from_raw(entity(*view)?),
                approved: *approved,
            },
            Self::ProviderAvailability {
                provider,
                available,
            } => C::SetProviderAvailability {
                provider: core::ProviderId::from_raw(entity(*provider)?),
                availability: if *available {
                    core::ProviderAvailability::Available
                } else {
                    core::ProviderAvailability::Missing
                },
            },
            Self::Viewport { .. } | Self::CommitPreview { .. } => {
                return Err(WebError::new("host_dispatch_required"))
            }
        })
    }
    fn validate(&self) -> Result<(), WebError> {
        match self {
            Self::Viewport { rect, scale } => {
                rect.to_rect()?;
                positive(*scale)?;
            }
            Self::Float { rect, frame } => {
                entity(*frame)?;
                rect.to_rect()?;
            }
            Self::Dock { frame, location } => {
                entity(*frame)?;
                location.to_location()?;
            }
            Self::MoveFrame {
                frame, destination, ..
            } => {
                entity(*frame)?;
                entity(*destination)?;
            }
            Self::MoveView {
                view, destination, ..
            } => {
                entity(*view)?;
                entity(*destination)?;
            }
            Self::ResizeSplit {
                workspace,
                node,
                weights: w,
            } => {
                entity(*workspace)?;
                entity(*node)?;
                weights(w)?;
            }
            Self::GridTracks {
                workspace,
                node,
                rows,
                columns,
            } => {
                entity(*workspace)?;
                entity(*node)?;
                weights(rows)?;
                weights(columns)?;
            }
            Self::SetLayout { workspace, plan } => {
                entity(*workspace)?;
                if plan.nodes.len() > MAX_LIST {
                    return Err(WebError::new("list_limit"));
                }
                for (id, node) in &plan.nodes {
                    entity(Decimal(id.0))?;
                    match node {
                        core::LayoutNode::Split { children, .. } => {
                            if children.is_empty() || children.len() > MAX_LIST {
                                return Err(WebError::new("list_limit"));
                            }
                            for c in children {
                                positive(c.weight.get())?;
                            }
                        }
                        core::LayoutNode::Grid {
                            rows,
                            columns,
                            cells,
                        } => {
                            weights(&rows.iter().map(|v| v.get()).collect::<Vec<_>>())?;
                            weights(&columns.iter().map(|v| v.get()).collect::<Vec<_>>())?;
                            if cells.len() > MAX_LIST {
                                return Err(WebError::new("list_limit"));
                            }
                        }
                        _ => {}
                    }
                }
            }
            Self::Select { view } | Self::Close { view, .. } => {
                entity(*view)?;
            }
            Self::Redock { frame } | Self::Maximize { frame } | Self::Expand { frame, .. } => {
                entity(*frame)?;
            }
            Self::Equalize { workspace } | Self::RestoreLayout { workspace } => {
                entity(*workspace)?;
            }
            Self::Swap { first, second } => {
                entity(*first)?;
                entity(*second)?;
            }
            Self::ProviderAvailability { provider, .. } => {
                entity(*provider)?;
            }
            Self::CommitPreview { token } => {
                if token.0 == 0 {
                    return Err(WebError::new("invalid_preview_token"));
                }
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum HostAction {
    Edit { title: String, body: String },
    Domain { column: String, status: String },
    Veto { enabled: bool },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "route", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    #[cfg(feature = "drops")]
    DropAdmit { version: u32, destination: DropDestinationWire, action: String, source: String, items: Vec<DropItemWire>, now_ms: u64 },
    #[cfg(feature = "drops")]
    DropComplete { version: u32, token: Decimal, contents: Vec<String>, now_ms: u64 },
    #[cfg(feature = "drops")]
    DropCancel { version: u32, token: Decimal },
    Snapshot {
        version: u32,
    },
    HostSnapshot {
        version: u32,
    },
    Save {
        version: u32,
    },
    Preview {
        version: u32,
        identity: core::Identity,
        expected_generation: Decimal,
        action: Action,
    },
    CancelPreview {
        version: u32,
        token: Decimal,
    },
    Command {
        version: u32,
        identity: core::Identity,
        actor: String,
        operation: Decimal,
        expected_generation: Decimal,
        action: Action,
    },
    HostCommand {
        version: u32,
        identity: core::Identity,
        actor: String,
        expected_revision: Decimal,
        view: Decimal,
        resource: Decimal,
        provider: Decimal,
        action: HostAction,
    },
    Load {
        version: u32,
        layout: String,
        host: String,
        fresh_incarnation: String,
    },
    Input {
        version: u32,
        identity: core::Identity,
        epoch: Decimal,
        view: Decimal,
        resource: Decimal,
        provider: Decimal,
        x: f64,
        y: f64,
        scale: f64,
        payload: String,
    },
}
impl Request {
    pub fn version(&self) -> u32 {
        match self {
            #[cfg(feature = "drops")]
            Self::DropAdmit { version, .. } | Self::DropComplete { version, .. } | Self::DropCancel { version, .. } => *version,
            Self::Snapshot { version }
            | Self::HostSnapshot { version }
            | Self::Save { version }
            | Self::Preview { version, .. }
            | Self::CancelPreview { version, .. }
            | Self::Command { version, .. }
            | Self::HostCommand { version, .. }
            | Self::Load { version, .. }
            | Self::Input { version, .. } => *version,
        }
    }
    fn validate(&self) -> Result<(), WebError> {
        if self.version() != VERSION {
            return Err(WebError::new("unsupported_route_version"));
        }
        match self {
            Self::Preview {
                identity, action, ..
            } => {
                identity_valid(identity)?;
                action.validate()?;
            }
            Self::Command {
                identity,
                actor,
                operation,
                action,
                ..
            } => {
                identity_valid(identity)?;
                actor_valid(actor)?;
                if operation.0 == 0 {
                    return Err(WebError::new("zero_operation"));
                }
                action.validate()?;
            }
            Self::HostCommand {
                identity,
                actor,
                view,
                resource,
                provider,
                action,
                ..
            } => {
                identity_valid(identity)?;
                actor_valid(actor)?;
                entity(*view)?;
                entity(*resource)?;
                entity(*provider)?;
                match action {
                    HostAction::Edit { title, body } => {
                        field(title)?;
                        field(body)?;
                    }
                    HostAction::Domain { column, status } => {
                        field(column)?;
                        field(status)?;
                    }
                    HostAction::Veto { .. } => {}
                }
            }
            Self::Load {
                fresh_incarnation, ..
            } => {
                if fresh_incarnation.is_empty() || fresh_incarnation.len() > 128 {
                    return Err(WebError::new("invalid_identity"));
                }
            }
            Self::Input {
                identity,
                view,
                resource,
                provider,
                x,
                y,
                scale,
                payload,
                ..
            } => {
                identity_valid(identity)?;
                entity(*view)?;
                entity(*resource)?;
                entity(*provider)?;
                if !x.is_finite() || !y.is_finite() {
                    return Err(WebError::new("invalid_coordinates"));
                }
                positive(*scale)?;
                field(payload)?;
            }
            Self::CancelPreview { token, .. } if token.0 == 0 => {
                return Err(WebError::new("invalid_preview_token"))
            }
            _ => {}
        }
        Ok(())
    }
}

#[cfg(feature = "drops")]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DropDestinationWire {
    pub identity: core::Identity, pub generation: Decimal, pub epoch: Decimal,
    pub frame: Decimal, pub view: Decimal, pub resource: Decimal, pub provider: Decimal,
    pub x: f64, pub y: f64,
}
#[cfg(feature = "drops")]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DropItemWire {
    pub label: String, pub media_type: String, pub bytes: Option<u64>,
    pub asset: Option<String>, pub asset_revision: Option<Decimal>,
}

pub fn parse_request(input: &str) -> Result<Request, WebError> {
    if input.len() > MAX_ROUTE_BYTES {
        return Err(WebError::new("route_size_limit"));
    }
    let mut deserializer = serde_json::Deserializer::from_str(input);
    let Strict(value) =
        Strict::deserialize(&mut deserializer).map_err(|_| WebError::new("invalid_json"))?;
    deserializer
        .end()
        .map_err(|_| WebError::new("invalid_json"))?;
    let request: Request =
        serde_json::from_value(value).map_err(|_| WebError::new("invalid_request"))?;
    request.validate()?;
    Ok(request)
}
fn entity(value: Decimal) -> Result<u64, WebError> {
    if value.0 == 0 || value.0 == u64::MAX {
        Err(WebError::new("invalid_entity_id"))
    } else {
        Ok(value.0)
    }
}
fn field(value: &str) -> Result<(), WebError> {
    if value.len() > MAX_FIELDS_BYTES {
        Err(WebError::new("field_size_limit"))
    } else {
        Ok(())
    }
}
fn actor_valid(actor: &str) -> Result<(), WebError> {
    if actor.is_empty() || actor.len() > 128 {
        Err(WebError::new("invalid_actor"))
    } else {
        Ok(())
    }
}
fn identity_valid(identity: &core::Identity) -> Result<(), WebError> {
    core::Identity::new(identity.lineage.clone(), identity.incarnation.clone())
        .map(|_| ())
        .map_err(|_| WebError::new("invalid_identity"))
}
fn positive(value: f64) -> Result<(), WebError> {
    if !value.is_finite() || value <= 0.0 {
        Err(WebError::new("invalid_positive_number"))
    } else {
        Ok(())
    }
}
fn scalar(value: f64) -> Result<core::Scalar, WebError> {
    positive(value)?;
    core::Scalar::new(value).map_err(|_| WebError::new("invalid_positive_number"))
}
fn weights(values: &[f64]) -> Result<(), WebError> {
    if values.is_empty() || values.len() > MAX_LIST {
        return Err(WebError::new("list_limit"));
    }
    for value in values {
        positive(*value)?;
    }
    Ok(())
}

// serde_json::Value normally silently retains the last duplicate. Check every object first.
struct Strict(Value);
impl<'de> Deserialize<'de> for Strict {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        d.deserialize_any(StrictVisitor)
    }
}
struct StrictVisitor;
impl<'de> Visitor<'de> for StrictVisitor {
    type Value = Strict;
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("JSON without duplicate members")
    }
    fn visit_bool<E: de::Error>(self, v: bool) -> Result<Strict, E> {
        Ok(Strict(Value::Bool(v)))
    }
    fn visit_i64<E: de::Error>(self, v: i64) -> Result<Strict, E> {
        Ok(Strict(v.into()))
    }
    fn visit_u64<E: de::Error>(self, v: u64) -> Result<Strict, E> {
        Ok(Strict(v.into()))
    }
    fn visit_f64<E: de::Error>(self, v: f64) -> Result<Strict, E> {
        serde_json::Number::from_f64(v)
            .map(|n| Strict(Value::Number(n)))
            .ok_or_else(|| E::custom("nonfinite"))
    }
    fn visit_str<E: de::Error>(self, v: &str) -> Result<Strict, E> {
        Ok(Strict(Value::String(v.into())))
    }
    fn visit_string<E: de::Error>(self, v: String) -> Result<Strict, E> {
        Ok(Strict(Value::String(v)))
    }
    fn visit_none<E: de::Error>(self) -> Result<Strict, E> {
        Ok(Strict(Value::Null))
    }
    fn visit_unit<E: de::Error>(self) -> Result<Strict, E> {
        Ok(Strict(Value::Null))
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> Result<Strict, A::Error> {
        let mut values = Vec::new();
        while let Some(Strict(v)) = a.next_element()? {
            values.push(v);
        }
        Ok(Strict(Value::Array(values)))
    }
    fn visit_map<A: MapAccess<'de>>(self, mut a: A) -> Result<Strict, A::Error> {
        let mut values = serde_json::Map::new();
        let mut seen = BTreeSet::new();
        while let Some(key) = a.next_key::<String>()? {
            if !seen.insert(key.clone()) {
                return Err(de::Error::custom("duplicate member"));
            }
            let Strict(value) = a.next_value()?;
            values.insert(key, value);
        }
        Ok(Strict(Value::Object(values)))
    }
}
