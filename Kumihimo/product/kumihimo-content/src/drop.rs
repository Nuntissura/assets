//! Optional metadata-only drop coordination. Payload handles and effects belong to the host.
use crate::*;
use kumihimo_core::FrameId;
use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DropPayloadKind { DiskFiles, BrowserFiles, InternalAssets }
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum DropAction { Open, Link, Import, Copy, Move }
impl DropAction {
    pub fn label(self) -> &'static str {
        match self { Self::Open => "Open", Self::Link => "Link", Self::Import => "Import", Self::Copy => "Copy", Self::Move => "Move" }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DropItem {
    /// Basename/label only. Never an absolute path or URL.
    pub label: String,
    pub media_type: String,
    /// None is valid during protected browser dragover.
    pub bytes: Option<u64>,
    pub asset: Option<HostAssetReference>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DropOffer {
    pub kind: DropPayloadKind,
    /// Opaque identifier into a host-owned payload table; no bytes or paths.
    pub token: String,
    pub revision: u64,
    pub expires_at_ms: u64,
    pub items: Vec<DropItem>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DropLimits { pub items: usize, pub metadata_bytes: usize, pub payload_bytes: u64 }
impl Default for DropLimits {
    fn default() -> Self { Self { items: 16, metadata_bytes: 8192, payload_bytes: 1024 * 1024 } }
}
impl DropOffer {
    pub fn opaque_bytes(&self) -> Result<usize> {
        self.items.iter().try_fold(self.token.len(), |n, i| {
            n.checked_add(i.label.len()).and_then(|n| n.checked_add(i.media_type.len()))
                .and_then(|n| n.checked_add(i.asset.as_ref().map_or(0, |a| a.identity.len() + a.revision.len())))
                .ok_or(Error::Exhausted("drop metadata"))
        })
    }
    pub fn validate(&self, limits: DropLimits, now: u64) -> Result<()> {
        if self.token.is_empty() || self.token.len() > 128 || !self.token.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b)) || self.revision == 0 {
            return Err(Error::Invalid("drop token/revision"));
        }
        if self.expires_at_ms <= now { return Err(Error::Stale("drop payload expired")); }
        if self.items.is_empty() || self.items.len() > limits.items {
            return Err(Error::Admission { dimension: "drop items", limit: limits.items, attempted: self.items.len() });
        }
        let bytes = self.opaque_bytes()?;
        if bytes > limits.metadata_bytes { return Err(Error::Admission { dimension: "drop metadata", limit: limits.metadata_bytes, attempted: bytes }); }
        let mut total = 0_u64;
        for item in &self.items {
            if item.label.len() > 256 || item.label.contains(['/', '\\']) || item.label.chars().any(char::is_control) || item.media_type.len() > 128 || item.media_type.chars().any(char::is_control) {
                return Err(Error::Invalid("drop metadata"));
            }
            if (self.kind == DropPayloadKind::InternalAssets) != item.asset.is_some() { return Err(Error::Invalid("drop source kind")); }
            if let Some(asset) = &item.asset {
                if asset.identity.is_empty() || asset.revision.is_empty() { return Err(Error::Invalid("drop asset reference")); }
            }
            total = total.checked_add(item.bytes.unwrap_or(0)).ok_or(Error::Exhausted("drop payload bytes"))?;
        }
        if total > limits.payload_bytes { return Err(Error::Invalid("drop payload limit")); }
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DropDestination { pub scope: Scope, pub frame: FrameId, pub generation: u64, pub epoch: u64 }
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DropGrant { pub permission: HostPermission, pub actions: BTreeSet<DropAction>, pub kinds: Vec<DropPayloadKind> }
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DropRequest { pub offer: DropOffer, pub destination: DropDestination, pub action: DropAction }
impl DropRequest {
    pub fn validate(&self, field_limit: Option<usize>) -> Result<()> {
        self.destination.scope.validate()?;
        self.offer.validate(DropLimits { items: usize::MAX, metadata_bytes: field_limit.unwrap_or(usize::MAX), payload_bytes: u64::MAX }, 0)?;
        if self.destination.epoch == 0 || self.destination.frame.raw() == 0 { return Err(Error::Invalid("drop destination")); }
        Ok(())
    }
}
/// One consumable preview: cancellation and release both remove it. No payload ownership.
#[derive(Default)]
pub struct DropCoordinator { preview: Option<DropRequest>, permission: Option<HostPermission> }
impl DropCoordinator {
    pub fn preview(&self) -> Option<&DropRequest> { self.preview.as_ref() }
    pub fn cancel(&mut self) { self.preview = None; self.permission = None; }
    pub fn hover(&mut self, controller: &Controller, request: DropRequest, grant: &DropGrant, limits: DropLimits, now: u64) -> Result<()> {
        self.cancel();
        controller.validate_drop(&request, grant, limits, now)?;
        self.permission = Some(grant.permission);
        self.preview = Some(request);
        Ok(())
    }
    /// The host re-reads its payload table and permission immediately before release.
    pub fn release(&mut self, controller: &mut Controller, live_offer: &DropOffer, grant: &DropGrant, limits: DropLimits, now: u64) -> Result<RequestId> {
        let request = self.preview.take().ok_or(Error::Stale("drop cancelled or consumed"))?;
        let permission = self.permission.take();
        if &request.offer != live_offer { return Err(Error::Stale("drop payload changed")); }
        if permission != Some(grant.permission) { return Err(Error::Stale("drop permission changed")); }
        controller.validate_drop(&request, grant, limits, now)?;
        controller.request(request.destination.scope.mount, RequestKind::Drop(request), grant.permission, now)
    }
}
impl Controller {
    pub fn validate_drop(&self, request: &DropRequest, grant: &DropGrant, limits: DropLimits, now: u64) -> Result<()> {
        request.offer.validate(limits, now)?;
        if !grant.permission.allowed { return Err(Error::PermissionDenied); }
        if !grant.actions.contains(&request.action) || !grant.kinds.contains(&request.offer.kind) { return Err(Error::Unsupported("drop action/source")); }
        self.state.permission(request.destination.scope.mount, grant.permission)?;
        self.drop_target_valid(request)?;
        self.state.eligible(&self.core, request.destination.scope.mount, &RequestKind::Drop(request.clone()))
    }
    fn drop_target_valid(&self, request: &DropRequest) -> Result<()> {
        validate_target(&self.state, self.snapshot(), request)
    }
}
pub(crate) fn validate_target(state: &crate::routing::State, snapshot: &kumihimo_core::Snapshot, request: &DropRequest) -> Result<()> {
    let destination = &request.destination;
    let mount = state.mounts.get(&destination.scope.mount).ok_or(Error::Missing("drop mount", destination.scope.mount.0))?;
    let view = destination.scope.target.view().and_then(|v| snapshot.views.get(&v)).ok_or(Error::Stale("drop view"))?;
    if destination.generation != snapshot.generation || mount.scope != destination.scope || !mount.active || !state.binding_valid(snapshot, &destination.scope) || view.frame != destination.frame || view.closed.is_some() || !view.state.visible || !view.state.usable || snapshot.frames.get(&view.frame).and_then(|f| f.selected_view) != Some(view.id) {
        return Err(Error::Stale("drop destination"));
    }
    Ok(())
}
