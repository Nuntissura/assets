//! Sample host policy: bounded UTF-8 text open/import/copy and internal task links.
use super::*;
use content::{DropAction, DropCoordinator, DropDestination, DropGrant, DropItem, DropLimits, DropOffer, DropPayloadKind, DropRequest};
const MAX_FILES: usize = 8;
const MAX_BYTES: u64 = 8192;
pub(super) struct Admission { coordinator: DropCoordinator, offer: DropOffer, revision: u64, started_ms: u64 }
fn limits() -> DropLimits { DropLimits { items: MAX_FILES, metadata_bytes: 4096, payload_bytes: MAX_BYTES } }
fn grant(source: DropPayloadKind) -> DropGrant {
    DropGrant { permission: permission(), actions: if source == DropPayloadKind::InternalAssets { BTreeSet::from([DropAction::Link, DropAction::Copy]) } else { BTreeSet::from([DropAction::Open, DropAction::Import, DropAction::Copy]) }, kinds: vec![source] }
}
impl PilotHost {
    pub(super) fn drop_admit(&mut self, d: DropDestinationWire, action: &str, source: &str, items: Vec<DropItemWire>, now_ms: u64) -> Result<Value, WebError> {
        // Drop admission is metadata only; the browser still owns all File handles.
        if self.drops.len() >= 16 { return Err(WebError::new("drop_pending_limit")); }
        if d.generation.0 != self.controller.snapshot().generation || d.epoch.0 != self.epoch { return Err(WebError::new("stale_drop_target")); }
        let mount = self.recipient(&d.identity, d.view.0, d.resource.0, d.provider.0)?;
        if self.tasks.tasks.iter().find(|t| t.resource.0 == d.resource.0 && t.views.iter().any(|v| v.0 == d.view.0)).is_none_or(|t| t.veto) { return Err(WebError::new("drop_host_refused")); }
        let snapshot = self.snapshot()?;
        let frame = snapshot["frames"].as_array().and_then(|frames| frames.iter().find(|f| f["id"].as_str() == Some(&d.frame.0.to_string()) && f["selected_view"].as_str() == Some(&d.view.0.to_string()))).ok_or_else(|| WebError::new("drop_target_mismatch"))?;
        let rect = rect_from_value(&frame["content"])?;
        let clip = intersect(rect, rect_from_value(&frame["clip"])?)?;
        if frame["visible"] != true || frame["expanded"] != true || !d.x.is_finite() || !d.y.is_finite() || d.x < clip.x() || d.x >= clip.right() || d.y < clip.y() || d.y >= clip.bottom() { return Err(WebError::new("drop_outside_content")); }
        // Canonical topmost pane check prevents a forged ancestor destination.
        let top = snapshot["frames"].as_array().unwrap().iter().filter(|f| {
            if f["visible"] != true { return false; }
            let Ok(r) = rect_from_value(&f["rect"]).and_then(|r| intersect(r, rect_from_value(&f["clip"])?)) else { return false; };
            d.x >= r.x() && d.x < r.right() && d.y >= r.y() && d.y < r.bottom()
        }).max_by_key(|f| (f["depth"].as_u64().unwrap_or(0), f["id"].as_str().and_then(|v| v.parse::<u64>().ok()).unwrap_or(0)));
        if top.is_none_or(|f| f["id"] != frame["id"]) { return Err(WebError::new("drop_occluded")); }
        let action = match action { "open" => DropAction::Open, "import" => DropAction::Import, "copy" => DropAction::Copy, "link" => DropAction::Link, _ => return Err(WebError::new("drop_action_unsupported")) };
        let kind = match source { "browser_files" => DropPayloadKind::BrowserFiles, "internal_assets" => DropPayloadKind::InternalAssets, _ => return Err(WebError::new("drop_source_unsupported")) };
        let token = self.next_drop;
        self.next_drop = token.checked_add(1).ok_or_else(|| WebError::new("drop_token_exhausted"))?;
        let items = items.into_iter().map(|i| DropItem { label: i.label, media_type: i.media_type, bytes: i.bytes,
            asset: i.asset.zip(i.asset_revision).map(|(identity, revision)| content::HostAssetReference { identity, revision: revision.0.to_string() }) }).collect::<Vec<_>>();
        if kind == DropPayloadKind::BrowserFiles && items.iter().any(|i| !matches!(i.media_type.as_str(), "text/plain" | "text/markdown" | "") || !(i.label.ends_with(".txt") || i.label.ends_with(".md")) || i.bytes.is_none()) { return Err(WebError::new("drop_format_unsupported")); }
        if kind == DropPayloadKind::InternalAssets && items.iter().any(|i| i.asset.as_ref().is_none_or(|a| a.revision != self.tasks.revision.0.to_string() || !self.tasks.tasks.iter().any(|t| t.id == a.identity))) { return Err(WebError::new("drop_asset_stale")); }
        let offer = DropOffer { kind, token: format!("browser-drop-{token}"), revision: 1, expires_at_ms: 30_000, items };
        let scope = self.controller.inspect().mounts[&mount].scope.clone();
        let mut coordinator = DropCoordinator::default();
        coordinator.hover(&self.controller, DropRequest { offer: offer.clone(), destination: DropDestination { scope, frame: core::FrameId::from_raw(d.frame.0), generation: d.generation.0, epoch: self.epoch }, action }, &grant(kind), limits(), 0)?;
        self.drops.insert(token, Admission { coordinator, offer, revision: self.tasks.revision.0, started_ms: now_ms });
        Ok(json!({"token":token.to_string(),"status":"pending","action":action.label(),"frame":d.frame,"view":d.view,"max_bytes":MAX_BYTES,"lifetime_ms":30_000}))
    }
    pub(super) fn drop_cancel(&mut self, token: u64) -> Result<Value, WebError> {
        let mut admission = self.drops.remove(&token).ok_or_else(|| WebError::new("drop_consumed"))?;
        admission.coordinator.cancel();
        Ok(json!({"token":token.to_string(),"status":"cancelled"}))
    }
    pub(super) fn drop_complete(&mut self, token: u64, contents: Vec<String>, now_ms: u64) -> Result<Value, WebError> {
        // Single consumption: failure, cancellation and success all prevent a second effect.
        let mut admission = self.drops.remove(&token).ok_or_else(|| WebError::new("drop_consumed"))?;
        if now_ms < admission.started_ms || now_ms - admission.started_ms >= 30_000 { return Err(WebError::new("drop_expired")); }
        let request = admission.coordinator.preview().cloned().ok_or_else(|| WebError::new("drop_cancelled"))?;
        if admission.revision != self.tasks.revision.0 || request.destination.epoch != self.epoch { return Err(WebError::new("stale_drop_host")); }
        let text = if admission.offer.kind == DropPayloadKind::InternalAssets {
            if !contents.is_empty() { return Err(WebError::new("drop_unexpected_content")); }
            admission.offer.items.iter().map(|i| self.tasks.tasks.iter().find(|t| i.asset.as_ref().is_some_and(|a| a.identity == t.id)).map(|t| if request.action == DropAction::Link { format!("Linked task: {}", t.id) } else { t.body.clone() }).ok_or_else(|| WebError::new("drop_asset_stale"))).collect::<Result<Vec<_>, _>>()?.join("\n")
        } else {
            if contents.len() != admission.offer.items.len() || contents.iter().zip(&admission.offer.items).any(|(text, item)| item.bytes != Some(text.len() as u64)) { return Err(WebError::new("drop_payload_mismatch")); }
            contents.join("\n")
        };
        if text.len() as u64 > MAX_BYTES { return Err(WebError::new("drop_payload_limit")); }
        let mount = request.destination.scope.mount;
        let id = admission.coordinator.release(&mut self.controller, &admission.offer, &grant(admission.offer.kind), limits(), 0)?;
        let effect = self.controller.dispatch(mount, permission(), 0)?.ok_or_else(|| WebError::new("drop_waiting_prerequisite"))?;
        if effect.ticket.id != id { return Err(WebError::new("drop_ordering")); }
        let result = (|| {
            let index = self.tasks.tasks.iter().position(|t| t.resource.0 == effect.ticket.scope.resource.raw() && effect.ticket.scope.target.view().is_some_and(|v| t.views.iter().any(|id| id.0 == v.raw()))).ok_or_else(|| WebError::new("drop_missing_task"))?;
            if self.tasks.tasks[index].veto { return Err(WebError::new("drop_host_refused")); }
            let body = if matches!(request.action, DropAction::Open | DropAction::Copy) { text } else { format!("{}\n{}", self.tasks.tasks[index].body, text) };
            if body.len() > 8192 { return Err(WebError::new("drop_document_limit")); }
            let revision = self.tasks.revision.0.checked_add(1).ok_or_else(|| WebError::new("drop_revision_exhausted"))?;
            if request.action == DropAction::Link {
                for item in &admission.offer.items {
                    let asset = item.asset.as_ref().ok_or_else(|| WebError::new("drop_asset_missing"))?;
                    self.tasks.tasks[index].links.insert(asset.identity.clone(), Decimal(admission.revision));
                }
            } else { self.tasks.tasks[index].body = body; }
            self.tasks.revision = Decimal(revision);
            self.tasks.last_recipient = Some(json!({"provider":effect.ticket.scope.provider.raw().to_string(),"resource":effect.ticket.scope.resource.raw().to_string(),"view":effect.ticket.scope.target.view().map(|v|v.raw().to_string()),"frame":request.destination.frame.raw().to_string(),"request":id.0.to_string(),"kind":"file_drop","action":request.action.label(),"bytes":self.tasks.tasks[index].body.len()}));
            Ok(())
        })();
        let receipt = self.controller.ack(content::Ack { ticket: effect.ticket, result: if result.is_ok() { content::AckResult::Confirmed(content::Confirmation::Done) } else { content::AckResult::Rejected } }, 0)?;
        result?;
        Ok(json!({"token":token.to_string(),"status":"completed","request":receipt.ticket.id.0.to_string(),"recipient":self.tasks.last_recipient,"revision":self.tasks.revision}))
    }
}
