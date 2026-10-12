//! Explicit host invalidation; the screenshot bridge has no animation/frame-ready feed.
use crate::frame::{BridgeError, BridgeLimits, Result, ScopeTarget};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkState {
    Static,
    Dirty,
    CapturePending,
    Hidden,
    Unloaded,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WorkCounts {
    pub dirty_marks: u64,
    pub capture_requests: u64,
    pub captures_completed: u64,
    pub texture_uploads: u64,
}

#[derive(Clone, Debug)]
struct ViewWork {
    visible: bool,
    loaded: bool,
    dirty: bool,
    pending: Option<u64>,
    last_request: u64,
    completed: Option<u64>,
    uploaded: Option<(u64, u64)>,
    counts: WorkCounts,
}

/// Only admitted live bridge views are tracked; host logical tabs stay separate.
#[derive(Clone, Debug)]
pub struct DirtyScheduler {
    limit: usize,
    views: BTreeMap<ScopeTarget, ViewWork>,
}
impl DirtyScheduler {
    pub fn new(limits: &BridgeLimits) -> Result<Self> {
        limits.validate()?;
        Ok(Self {
            limit: limits.max_host_frames,
            views: BTreeMap::new(),
        })
    }
    pub fn register(&mut self, target: ScopeTarget, visible: bool, loaded: bool) -> Result<()> {
        if target.raw() == 0 || target.raw() == u64::MAX {
            return Err(BridgeError::Invalid("scheduler target"));
        }
        if self.views.contains_key(&target) {
            return Err(BridgeError::Duplicate);
        }
        if self.views.len() >= self.limit {
            return Err(BridgeError::Budget("scheduled host frames"));
        }
        self.views.insert(
            target,
            ViewWork {
                visible,
                loaded,
                dirty: loaded,
                pending: None,
                last_request: 0,
                completed: None,
                uploaded: None,
                counts: WorkCounts::default(),
            },
        );
        Ok(())
    }
    pub fn remove(&mut self, target: ScopeTarget) -> bool {
        self.views.remove(&target).is_some()
    }
    pub fn mark_dirty(&mut self, target: ScopeTarget) -> Result<()> {
        let view = self
            .views
            .get_mut(&target)
            .ok_or(BridgeError::UnknownView)?;
        let marks = view
            .counts
            .dirty_marks
            .checked_add(1)
            .ok_or(BridgeError::Exhausted("dirty marks"))?;
        view.counts.dirty_marks = marks;
        view.dirty = true;
        Ok(())
    }
    pub fn set_visible(&mut self, target: ScopeTarget, visible: bool) -> Result<()> {
        self.views
            .get_mut(&target)
            .ok_or(BridgeError::UnknownView)?
            .visible = visible;
        Ok(())
    }
    pub fn set_loaded(&mut self, target: ScopeTarget, loaded: bool) -> Result<()> {
        let view = self
            .views
            .get_mut(&target)
            .ok_or(BridgeError::UnknownView)?;
        if view.loaded != loaded {
            view.loaded = loaded;
            view.pending = None;
            view.dirty = loaded;
            view.completed = None;
            view.uploaded = None;
        }
        Ok(())
    }
    pub fn state(&self, target: ScopeTarget) -> Result<WorkState> {
        let v = self.views.get(&target).ok_or(BridgeError::UnknownView)?;
        Ok(if !v.loaded {
            WorkState::Unloaded
        } else if !v.visible {
            WorkState::Hidden
        } else if v.pending.is_some() {
            WorkState::CapturePending
        } else if v.dirty {
            WorkState::Dirty
        } else {
            WorkState::Static
        })
    }
    pub fn begin_capture(&mut self, target: ScopeTarget, request_id: u64) -> Result<bool> {
        let v = self
            .views
            .get_mut(&target)
            .ok_or(BridgeError::UnknownView)?;
        if v.pending.is_some() {
            return Err(BridgeError::Busy);
        }
        if !v.loaded || !v.visible || !v.dirty {
            return Ok(false);
        }
        if request_id == 0 || request_id <= v.last_request {
            return Err(BridgeError::Duplicate);
        }
        let count = v
            .counts
            .capture_requests
            .checked_add(1)
            .ok_or(BridgeError::Exhausted("capture requests"))?;
        v.counts.capture_requests = count;
        v.last_request = request_id;
        v.pending = Some(request_id);
        v.dirty = false;
        Ok(true)
    }
    /// Dirty marks that arrive during a capture survive its completion.
    pub fn finish_capture(
        &mut self,
        target: ScopeTarget,
        request_id: u64,
        accepted: bool,
    ) -> Result<()> {
        let v = self
            .views
            .get_mut(&target)
            .ok_or(BridgeError::UnknownView)?;
        if v.pending != Some(request_id) {
            return Err(BridgeError::Stale("capture completion"));
        }
        if accepted {
            let count = v
                .counts
                .captures_completed
                .checked_add(1)
                .ok_or(BridgeError::Exhausted("capture completions"))?;
            v.counts.captures_completed = count;
            v.completed = Some(request_id);
        } else {
            v.dirty = true;
        }
        v.pending = None;
        Ok(())
    }
    /// A renderer replacement can upload the same capture once to its new epoch.
    pub fn record_upload(
        &mut self,
        target: ScopeTarget,
        request_id: u64,
        surface_epoch: u64,
    ) -> Result<bool> {
        let v = self
            .views
            .get_mut(&target)
            .ok_or(BridgeError::UnknownView)?;
        if surface_epoch == 0 {
            return Err(BridgeError::Invalid("upload surface epoch"));
        }
        if !v.loaded || v.completed != Some(request_id) {
            return Err(BridgeError::Stale("upload capture"));
        }
        if v.uploaded == Some((request_id, surface_epoch)) {
            return Ok(false);
        }
        if v.uploaded.is_some_and(|(_, epoch)| surface_epoch < epoch) {
            return Err(BridgeError::Stale("upload surface epoch"));
        }
        let count = v
            .counts
            .texture_uploads
            .checked_add(1)
            .ok_or(BridgeError::Exhausted("texture uploads"))?;
        v.counts.texture_uploads = count;
        v.uploaded = Some((request_id, surface_epoch));
        Ok(true)
    }
    pub fn counts(&self, target: ScopeTarget) -> Result<WorkCounts> {
        Ok(self
            .views
            .get(&target)
            .ok_or(BridgeError::UnknownView)?
            .counts)
    }
    pub fn pending(&self, target: ScopeTarget) -> Result<Option<u64>> {
        Ok(self
            .views
            .get(&target)
            .ok_or(BridgeError::UnknownView)?
            .pending)
    }
}
