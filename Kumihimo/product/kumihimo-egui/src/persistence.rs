//! Host-stored session metadata; schema-1 Core layout and native handles stay separate.
use crate::*;
use kumihimo_core::{DockLocation, GroupId, Identity, PersistedLayout, Snapshot};
use std::collections::BTreeSet;
pub const DESKTOP_SESSION_VERSION: u32 = 1;
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NativeContent {
    Frame(FrameId),
    View(ViewId),
    Group(GroupId),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReturnAnchor {
    Frame {
        workspace: WorkspaceId,
        location: DockLocation,
    },
    View {
        frame: FrameId,
        index: usize,
    },
    Group {
        frame: FrameId,
        index: usize,
        group_index: usize,
    },
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeBinding {
    pub root: WorkspaceId,
    pub carrier: Option<FrameId>,
    pub content: NativeContent,
    pub return_anchor: ReturnAnchor,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DesktopSession {
    pub version: u32,
    pub source: Identity,
    pub source_generation: u64,
    pub max_native_slots: usize,
    pub bindings: Vec<NativeBinding>,
}
impl DesktopSession {
    pub fn capture(
        snapshot: &Snapshot,
        max_native_slots: usize,
        bindings: Vec<NativeBinding>,
    ) -> Result<Self> {
        let value = Self {
            version: DESKTOP_SESSION_VERSION,
            source: snapshot.identity.clone(),
            source_generation: snapshot.generation,
            max_native_slots,
            bindings,
        };
        value.validate_current(snapshot)?;
        Ok(value)
    }
    pub fn validate_current(&self, snapshot: &Snapshot) -> Result<()> {
        if self.source != snapshot.identity || self.source_generation != snapshot.generation {
            return Err(AdapterError::Stale("session source identity/generation"));
        }
        self.validate_bindings(snapshot)
    }
    pub fn validate_after_restore(
        &self,
        layout: &PersistedLayout,
        snapshot: &Snapshot,
        fresh: &Identity,
    ) -> Result<()> {
        if layout.schema != kumihimo_core::SAVED_SCHEMA
            || self.source != layout.source
            || self.source_generation != layout.source_generation
            || &snapshot.identity != fresh
            || snapshot.identity.lineage != self.source.lineage
            || snapshot.identity.incarnation == self.source.incarnation
        {
            return Err(AdapterError::Stale(
                "session saved source/restored incarnation",
            ));
        }
        self.validate_bindings(snapshot)
    }
    pub fn rebind_after_restore(
        &self,
        layout: &PersistedLayout,
        snapshot: &Snapshot,
        fresh: &Identity,
    ) -> Result<Self> {
        self.validate_after_restore(layout, snapshot, fresh)?;
        Self::capture(snapshot, self.max_native_slots, self.bindings.clone())
    }
    fn validate_bindings(&self, snapshot: &Snapshot) -> Result<()> {
        if self.version != DESKTOP_SESSION_VERSION
            || self.max_native_slots == 0
            || self.bindings.len() > self.max_native_slots
        {
            return Err(AdapterError::Invalid("session version/capacity"));
        }
        let mut roots = BTreeSet::new();
        let mut carriers = BTreeSet::new();
        let mut entities = BTreeSet::new();
        for binding in &self.bindings {
            let root = snapshot
                .workspaces
                .get(&binding.root)
                .ok_or(AdapterError::Invalid("foreign native root"))?;
            if root.owner.is_some()
                || !roots.insert(binding.root)
                || snapshot.maximized.contains_key(&binding.root)
            {
                return Err(AdapterError::Invalid(
                    "owned/duplicate/maximized native root",
                ));
            }
            if root.frames.len() != 1 {
                return Err(AdapterError::Invalid(
                    "occupied or empty committed native root",
                ));
            }
            if let Some(carrier) = binding.carrier {
                let frame = snapshot
                    .frames
                    .get(&carrier)
                    .ok_or(AdapterError::Invalid("foreign carrier"))?;
                if frame.workspace != binding.root
                    || frame.child_workspace.is_some()
                    || !carriers.insert(carrier)
                {
                    return Err(AdapterError::Invalid("carrier association/ownership"));
                }
            }
            let (kind, id) = match binding.content {
                NativeContent::Frame(frame) => {
                    let f = snapshot
                        .frames
                        .get(&frame)
                        .ok_or(AdapterError::Invalid("foreign native frame"))?;
                    if f.workspace != binding.root || binding.carrier.is_some() {
                        return Err(AdapterError::Invalid("native frame/root association"));
                    }
                    (0u8, frame.raw())
                }
                NativeContent::View(view) => {
                    let v = snapshot
                        .views
                        .get(&view)
                        .ok_or(AdapterError::Invalid("foreign native view"))?;
                    if v.closed.is_some() || Some(v.frame) != binding.carrier {
                        return Err(AdapterError::Invalid("native view/carrier association"));
                    }
                    if snapshot.frames[&v.frame].views.iter().any(|id| *id != view) {
                        return Err(AdapterError::Invalid("occupied view carrier"));
                    }
                    (1, view.raw())
                }
                NativeContent::Group(group) => {
                    let g = snapshot
                        .groups
                        .get(&group)
                        .ok_or(AdapterError::Invalid("foreign native group"))?;
                    if Some(g.frame) != binding.carrier
                        || snapshot.frames[&g.frame]
                            .views
                            .iter()
                            .any(|id| snapshot.views[id].group != Some(group))
                    {
                        return Err(AdapterError::Invalid("group carrier association/occupancy"));
                    }
                    (2, group.raw())
                }
            };
            if !entities.insert((kind, id)) {
                return Err(AdapterError::Invalid("duplicate native content"));
            }
            // Return anchors can disappear while the window remains recoverable; shape must
            // match the native content. Fresh return eligibility is checked at action time.
            if !matches!(
                (&binding.content, &binding.return_anchor),
                (NativeContent::Frame(_), ReturnAnchor::Frame { .. })
                    | (NativeContent::View(_), ReturnAnchor::View { .. })
                    | (NativeContent::Group(_), ReturnAnchor::Group { .. })
            ) {
                return Err(AdapterError::Invalid("return anchor content kind"));
            }
            if matches!(
                binding.return_anchor,
                ReturnAnchor::Frame {
                    location: DockLocation::Cell { .. },
                    ..
                }
            ) {
                return Err(AdapterError::Invalid("plan-local return anchor"));
            }
        }
        Ok(())
    }
    #[cfg(feature = "capture")]
    pub fn to_json(&self, max_bytes: usize) -> Result<Vec<u8>> {
        use serde_json::json;
        if self.bindings.iter().any(|b| {
            matches!(
                b.return_anchor,
                ReturnAnchor::Frame {
                    location: DockLocation::Cell { .. },
                    ..
                }
            )
        }) {
            return Err(AdapterError::Invalid("plan-local return anchor"));
        }
        let bindings: Vec<_> = self.bindings.iter().map(|b| {
            let (kind,id) = match b.content { NativeContent::Frame(f)=>("frame",f.raw()), NativeContent::View(v)=>("view",v.raw()), NativeContent::Group(g)=>("group",g.raw()) };
            let anchor = match b.return_anchor {
                ReturnAnchor::Frame{workspace,location} => json!({"kind":"frame","workspace":workspace.raw().to_string(),"location": dock_json(location)}),
                ReturnAnchor::View{frame,index} => json!({"kind":"view","frame":frame.raw().to_string(),"index":index}),
                ReturnAnchor::Group{frame,index,group_index} => json!({"kind":"group","frame":frame.raw().to_string(),"index":index,"group_index":group_index}),
            };
            json!({"root":b.root.raw().to_string(),"carrier":b.carrier.map(|f|f.raw().to_string()),"kind":kind,"content":id.to_string(),"return":anchor})
        }).collect();
        diagnostics::encode_bounded(
            &json!({"version":self.version,"source":{"lineage":self.source.lineage,"incarnation":self.source.incarnation},"source_generation":self.source_generation.to_string(),"max_native_slots":self.max_native_slots,"bindings":bindings}),
            max_bytes,
        )
    }
    #[cfg(feature = "capture")]
    pub fn from_json(bytes: &[u8], max_bytes: usize) -> Result<Self> {
        use serde_json::Value;
        if bytes.len() > max_bytes {
            return Err(AdapterError::Budget("session bytes"));
        }
        let value: Value =
            serde_json::from_slice(bytes).map_err(|_| AdapterError::Invalid("session JSON"))?;
        exact_keys(
            &value,
            &[
                "version",
                "source",
                "source_generation",
                "max_native_slots",
                "bindings",
            ],
        )?;
        let source = &value["source"];
        exact_keys(source, &["lineage", "incarnation"])?;
        let identity = Identity::new(string(source, "lineage")?, string(source, "incarnation")?)?;
        let capacity = number(&value, "max_native_slots")?;
        let rows = value["bindings"]
            .as_array()
            .ok_or(AdapterError::Invalid("session bindings"))?;
        if capacity == 0 || rows.len() > capacity {
            return Err(AdapterError::Budget("native slots"));
        }
        let mut bindings = Vec::with_capacity(rows.len());
        for row in rows {
            exact_keys(row, &["root", "carrier", "kind", "content", "return"])?;
            let root = WorkspaceId::from_raw(id(row, "root")?);
            let carrier = if row["carrier"].is_null() {
                None
            } else {
                Some(FrameId::from_raw(id(row, "carrier")?))
            };
            let raw = id(row, "content")?;
            let content = match string(row, "kind")? {
                "frame" => NativeContent::Frame(FrameId::from_raw(raw)),
                "view" => NativeContent::View(ViewId::from_raw(raw)),
                "group" => NativeContent::Group(GroupId::from_raw(raw)),
                _ => return Err(AdapterError::Invalid("session content kind")),
            };
            let anchor = &row["return"];
            let return_anchor = match string(anchor, "kind")? {
                "frame" => {
                    exact_keys(anchor, &["kind", "workspace", "location"])?;
                    ReturnAnchor::Frame {
                        workspace: WorkspaceId::from_raw(id(anchor, "workspace")?),
                        location: parse_dock(&anchor["location"])?,
                    }
                }
                "view" => {
                    exact_keys(anchor, &["kind", "frame", "index"])?;
                    ReturnAnchor::View {
                        frame: FrameId::from_raw(id(anchor, "frame")?),
                        index: number(anchor, "index")?,
                    }
                }
                "group" => {
                    exact_keys(anchor, &["kind", "frame", "index", "group_index"])?;
                    ReturnAnchor::Group {
                        frame: FrameId::from_raw(id(anchor, "frame")?),
                        index: number(anchor, "index")?,
                        group_index: number(anchor, "group_index")?,
                    }
                }
                _ => return Err(AdapterError::Invalid("session return kind")),
            };
            bindings.push(NativeBinding {
                root,
                carrier,
                content,
                return_anchor,
            });
        }
        let version = u32::try_from(number(&value, "version")?)
            .map_err(|_| AdapterError::Invalid("session version"))?;
        if version != DESKTOP_SESSION_VERSION {
            return Err(AdapterError::Invalid("session version"));
        }
        Ok(Self {
            version,
            source: identity,
            source_generation: decimal(&value, "source_generation")?,
            max_native_slots: capacity,
            bindings,
        })
    }
}
#[cfg(feature = "capture")]
fn exact_keys(value: &serde_json::Value, keys: &[&str]) -> Result<()> {
    let map = value
        .as_object()
        .ok_or(AdapterError::Invalid("session object"))?;
    if map.len() != keys.len() || !keys.iter().all(|k| map.contains_key(*k)) {
        return Err(AdapterError::Invalid("session fields"));
    }
    Ok(())
}
#[cfg(feature = "capture")]
fn string<'a>(v: &'a serde_json::Value, k: &str) -> Result<&'a str> {
    v[k].as_str().ok_or(AdapterError::Invalid("session string"))
}
#[cfg(feature = "capture")]
fn number(v: &serde_json::Value, k: &str) -> Result<usize> {
    v[k].as_u64()
        .and_then(|n| usize::try_from(n).ok())
        .ok_or(AdapterError::Invalid("session number"))
}
#[cfg(feature = "capture")]
fn decimal(v: &serde_json::Value, k: &str) -> Result<u64> {
    let s = string(v, k)?;
    if s.is_empty() || (s.len() > 1 && s.starts_with('0')) || !s.bytes().all(|b| b.is_ascii_digit())
    {
        return Err(AdapterError::Invalid("session decimal"));
    }
    s.parse()
        .map_err(|_| AdapterError::Invalid("session decimal"))
}
#[cfg(feature = "capture")]
fn id(v: &serde_json::Value, k: &str) -> Result<u64> {
    let raw = decimal(v, k)?;
    if raw == 0 || raw == u64::MAX {
        Err(AdapterError::Invalid("session ID"))
    } else {
        Ok(raw)
    }
}
#[cfg(feature = "capture")]
fn parse_dock(v: &serde_json::Value) -> Result<DockLocation> {
    let edge = |v: &serde_json::Value| -> Result<kumihimo_core::Edge> {
        Ok(match string(v, "edge")? {
            "top" => kumihimo_core::Edge::Top,
            "bottom" => kumihimo_core::Edge::Bottom,
            "left" => kumihimo_core::Edge::Left,
            "right" => kumihimo_core::Edge::Right,
            _ => return Err(AdapterError::Invalid("return edge")),
        })
    };
    Ok(match string(v, "kind")? {
        "append" => {
            exact_keys(v, &["kind", "workspace"])?;
            DockLocation::Append {
                workspace: WorkspaceId::from_raw(id(v, "workspace")?),
            }
        }
        "edge" => {
            exact_keys(v, &["kind", "workspace", "edge"])?;
            DockLocation::WorkspaceEdge {
                workspace: WorkspaceId::from_raw(id(v, "workspace")?),
                edge: edge(v)?,
            }
        }
        "beside" => {
            exact_keys(v, &["kind", "frame", "edge"])?;
            DockLocation::Beside {
                frame: FrameId::from_raw(id(v, "frame")?),
                edge: edge(v)?,
            }
        }
        _ => return Err(AdapterError::Invalid("non-durable return dock")),
    })
}
#[cfg(feature = "capture")]
fn dock_json(location: DockLocation) -> serde_json::Value {
    use serde_json::json;
    let edge = |e| match e {
        kumihimo_core::Edge::Top => "top",
        kumihimo_core::Edge::Bottom => "bottom",
        kumihimo_core::Edge::Left => "left",
        kumihimo_core::Edge::Right => "right",
    };
    match location {
        DockLocation::Append { workspace } => {
            json!({"kind":"append","workspace":workspace.raw().to_string()})
        }
        DockLocation::WorkspaceEdge { workspace, edge: e } => {
            json!({"kind":"edge","workspace":workspace.raw().to_string(),"edge":edge(e)})
        }
        DockLocation::Beside { frame, edge: e } => {
            json!({"kind":"beside","frame":frame.raw().to_string(),"edge":edge(e)})
        }
        // Plan-local cell selectors cannot be saved as durable return anchors.
        DockLocation::Cell { .. } => serde_json::Value::Null,
    }
}
