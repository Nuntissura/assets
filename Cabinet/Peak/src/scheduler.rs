//! Effect-free host scheduling. Cancellation intent never frees a running worker slot.
use crate::{Content, Key, Peak, PreviewCompletion, PreviewRequest, Settings};
use std::collections::VecDeque;

#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SchedulerSettings {
    pub max_queued: usize,
    pub max_concurrent: usize,
    pub max_cache_bytes: usize,
    pub max_cache_entries: usize,
}
impl Default for SchedulerSettings {
    fn default() -> Self {
        Self {
            max_queued: 16,
            max_concurrent: 2,
            max_cache_bytes: 32 * 1024 * 1024,
            max_cache_entries: 128,
        }
    }
}
impl SchedulerSettings {
    pub fn valid(&self) -> bool {
        (1..=1024).contains(&self.max_queued)
            && (1..=64).contains(&self.max_concurrent)
            && self.max_cache_bytes <= 1024 * 1024 * 1024
            && self.max_cache_entries <= 4096
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SchedulerError {
    InvalidSettings,
    QueueFull,
    InvalidRequest,
    TokenExhausted,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Submission {
    Queued { token: u64 },
    Cached,
}
#[derive(Clone, Debug)]
pub struct PreviewJob {
    pub token: u64,
    pub request: PreviewRequest,
    pub settings: Settings,
}
#[derive(Clone, Debug)]
struct Active {
    job: PreviewJob,
    cancelled: bool,
    cancellation_sent: bool,
}
#[derive(Clone, Debug)]
struct Cached {
    key: Key,
    settings: Settings,
    content: Content,
    bytes: usize,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SchedulerDiagnostics {
    pub queued: usize,
    pub active: usize,
    pub cancelling: usize,
    pub cached: usize,
    pub cache_bytes: usize,
}
/// Own one scheduler per Peak interaction context. Hosts own the workers identified by tokens.
#[derive(Debug)]
pub struct PreviewScheduler {
    settings: SchedulerSettings,
    queued: VecDeque<PreviewJob>,
    active: Vec<Active>,
    cache: VecDeque<Cached>,
    cache_bytes: usize,
    sequence: u64,
    workspace: Option<(String, u64)>,
}
impl Default for PreviewScheduler {
    fn default() -> Self {
        Self::new(SchedulerSettings::default()).unwrap()
    }
}
impl PreviewScheduler {
    pub fn new(settings: SchedulerSettings) -> Result<Self, SchedulerError> {
        if !settings.valid() {
            return Err(SchedulerError::InvalidSettings);
        }
        Ok(Self {
            settings,
            queued: VecDeque::new(),
            active: vec![],
            cache: VecDeque::new(),
            cache_bytes: 0,
            sequence: 0,
            workspace: None,
        })
    }
    pub fn settings(&self) -> &SchedulerSettings {
        &self.settings
    }
    pub fn diagnostics(&self) -> SchedulerDiagnostics {
        SchedulerDiagnostics {
            queued: self.queued.len(),
            active: self.active.len(),
            cancelling: self.active.iter().filter(|a| a.cancelled).count(),
            cached: self.cache.len(),
            cache_bytes: self.cache_bytes,
        }
    }
    /// Reducing concurrency leaves existing workers owned until they actually finish.
    pub fn configure(&mut self, settings: SchedulerSettings) -> Result<(), SchedulerError> {
        if !settings.valid() {
            return Err(SchedulerError::InvalidSettings);
        }
        if self.queued.len() > settings.max_queued {
            return Err(SchedulerError::QueueFull);
        }
        self.settings = settings;
        self.evict();
        Ok(())
    }
    pub fn enqueue(
        &mut self,
        request: PreviewRequest,
        mut settings: Settings,
    ) -> Result<(Submission, Option<PreviewCompletion>), SchedulerError> {
        settings.max_input_bytes = settings.max_input_bytes.min(request.max_input_bytes);
        settings.max_text_lines = settings.max_text_lines.min(request.max_text_lines);
        settings.max_output_pixels = settings.max_output_pixels.min(request.max_output_pixels);
        if !settings.valid()
            || request.name.len() > 65536
            || request.key.scope.len() > 65536
            || request.key.entry_id.len() > 65536
            || request.key.resource_id.len() > 65536
        {
            return Err(SchedulerError::InvalidRequest);
        }
        self.bind_workspace(&request.key);
        if let Some(index) = self
            .cache
            .iter()
            .position(|c| c.key == request.key && c.settings == settings)
        {
            let cached = self.cache.remove(index).unwrap();
            let completion = PreviewCompletion {
                request_id: request.id,
                key: request.key,
                content: cached.content.clone(),
            };
            self.cache.push_back(cached);
            return Ok((Submission::Cached, Some(completion)));
        }
        if self.queued.len() >= self.settings.max_queued {
            return Err(SchedulerError::QueueFull);
        }
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or(SchedulerError::TokenExhausted)?;
        let token = self.sequence;
        self.queued.push_back(PreviewJob {
            token,
            request,
            settings,
        });
        Ok((Submission::Queued { token }, None))
    }
    /// Starts at most the configured number of owned workers. Invoke decoding off the UI thread.
    pub fn take_ready(&mut self) -> Vec<PreviewJob> {
        let mut ready = vec![];
        while self.active.len() < self.settings.max_concurrent {
            let Some(job) = self.queued.pop_front() else {
                break;
            };
            self.active.push(Active {
                job: job.clone(),
                cancelled: false,
                cancellation_sent: false,
            });
            ready.push(job);
        }
        ready
    }
    /// Discard queued stale work and request active cancellation; active slots remain occupied.
    pub fn retain_key(&mut self, key: Option<&Key>) {
        if let Some(key) = key {
            self.bind_workspace(key);
        }
        self.queued.retain(|j| key == Some(&j.request.key));
        for active in &mut self.active {
            if key != Some(&active.job.request.key) {
                active.cancelled = true;
            }
        }
    }
    pub fn cancel_all(&mut self) {
        self.retain_key(None);
    }
    pub fn take_cancellations(&mut self) -> Vec<u64> {
        self.active
            .iter_mut()
            .filter_map(|a| {
                if a.cancelled && !a.cancellation_sent {
                    a.cancellation_sent = true;
                    Some(a.job.token)
                } else {
                    None
                }
            })
            .collect()
    }
    /// Call only after the host proves this worker has stopped, never merely after sending cancellation.
    pub fn acknowledge_cancelled(&mut self, token: u64) -> bool {
        if let Some(index) = self
            .active
            .iter()
            .position(|a| a.job.token == token && a.cancelled)
        {
            self.active.remove(index);
            true
        } else {
            false
        }
    }
    pub fn finish(&mut self, token: u64, content: Content) -> Option<PreviewCompletion> {
        let index = self.active.iter().position(|a| a.job.token == token)?;
        let active = self.active.remove(index);
        if active.cancelled {
            return None;
        }
        let job = active.job;
        let content = match crate::validate_content(&content, &job.settings) {
            Ok(()) => content,
            Err(reason) => Content::Failed { reason },
        };
        // Transient failures are retried, rather than cached indefinitely.
        if !matches!(content, Content::Failed { .. }) {
            let bytes = content_bytes(&content)
                .saturating_add(job.request.key.scope.len())
                .saturating_add(job.request.key.entry_id.len())
                .saturating_add(job.request.key.resource_id.len())
                .saturating_add(256);
            if bytes <= self.settings.max_cache_bytes && self.settings.max_cache_entries > 0 {
                if let Some(index) = self
                    .cache
                    .iter()
                    .position(|c| c.key == job.request.key && c.settings == job.settings)
                {
                    let old = self.cache.remove(index).unwrap();
                    self.cache_bytes -= old.bytes;
                }
                self.cache_bytes = self.cache_bytes.saturating_add(bytes);
                self.cache.push_back(Cached {
                    key: job.request.key.clone(),
                    settings: job.settings,
                    content: content.clone(),
                    bytes,
                });
                self.evict();
            }
        }
        Some(PreviewCompletion {
            request_id: job.request.id,
            key: job.request.key,
            content,
        })
    }
    pub fn clear_cache(&mut self) {
        self.cache.clear();
        self.cache_bytes = 0;
    }
    fn bind_workspace(&mut self, key: &Key) {
        if self
            .workspace
            .as_ref()
            .is_some_and(|(scope, generation)| scope == &key.scope && *generation == key.generation)
        {
            return;
        }
        self.clear_cache();
        self.workspace = Some((key.scope.clone(), key.generation));
        self.queued.retain(|j| {
            j.request.key.scope == key.scope && j.request.key.generation == key.generation
        });
        for active in &mut self.active {
            if active.job.request.key.scope != key.scope
                || active.job.request.key.generation != key.generation
            {
                active.cancelled = true;
            }
        }
    }
    fn evict(&mut self) {
        while self.cache_bytes > self.settings.max_cache_bytes
            || self.cache.len() > self.settings.max_cache_entries
        {
            let Some(old) = self.cache.pop_front() else {
                break;
            };
            self.cache_bytes -= old.bytes;
        }
    }
}
fn content_bytes(content: &Content) -> usize {
    match content {
        Content::Text { body, .. } => body.len(),
        Content::Image { rgba, .. } => rgba.len(),
        Content::Unsupported { reason } | Content::Failed { reason } => reason.len(),
    }
}
impl Peak {
    /// Integrates hover timing, stale cancellation and cache replay; worker dispatch remains explicit.
    pub fn tick_scheduled(
        &mut self,
        scheduler: &mut PreviewScheduler,
        now_ms: u64,
    ) -> Result<Option<Submission>, SchedulerError> {
        let request = self.tick(now_ms);
        scheduler.retain_key(self.key());
        let Some(request) = request else {
            return Ok(None);
        };
        match scheduler.enqueue(request.clone(), self.settings.clone()) {
            Ok((submission, completion)) => {
                if let Some(completion) = completion {
                    self.complete(completion);
                }
                Ok(Some(submission))
            }
            Err(error) => {
                self.complete(PreviewCompletion {
                    request_id: request.id,
                    key: request.key,
                    content: Content::Failed {
                        reason: format!("Preview scheduling failed: {error:?}"),
                    },
                });
                Err(error)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn key(revision: u64) -> Key {
        Key {
            scope: "s".into(),
            generation: 1,
            context_revision: 1,
            entry_id: "a".into(),
            resource_id: "r".into(),
            entry_revision: revision,
        }
    }
    #[test]
    fn hover_dispatch_cache_and_revision_workflow() {
        let mut peak = Peak::default();
        let mut scheduler = PreviewScheduler::default();
        peak.hover(key(1), "a.rs", vec![], 0);
        assert!(matches!(
            peak.tick_scheduled(&mut scheduler, 1650).unwrap(),
            Some(Submission::Queued { .. })
        ));
        let job = scheduler.take_ready().pop().unwrap();
        let completion = scheduler
            .finish(
                job.token,
                crate::decode_text(b"hello", &job.settings, false),
            )
            .unwrap();
        assert!(peak.complete(completion));
        peak.leave(1700);
        peak.tick_scheduled(&mut scheduler, 2000).unwrap();
        peak.hover(key(1), "a.rs", vec![], 3000);
        assert_eq!(
            peak.tick_scheduled(&mut scheduler, 4650).unwrap(),
            Some(Submission::Cached)
        );
        assert!(scheduler.take_ready().is_empty());
        peak.hover(key(2), "a.rs", vec![], 5000);
        assert!(matches!(
            peak.tick_scheduled(&mut scheduler, 6650).unwrap(),
            Some(Submission::Queued { .. })
        ));
    }
    #[test]
    fn cancellation_retains_worker_slot_until_finish() {
        let mut scheduler = PreviewScheduler::new(SchedulerSettings {
            max_concurrent: 1,
            ..Default::default()
        })
        .unwrap();
        let mut peak = Peak::default();
        peak.hover(key(1), "a", vec![], 0);
        peak.tick_scheduled(&mut scheduler, 1650).unwrap();
        let job = scheduler.take_ready().pop().unwrap();
        peak.hover(key(2), "a", vec![], 2000);
        peak.tick_scheduled(&mut scheduler, 3650).unwrap();
        assert_eq!(scheduler.take_cancellations(), vec![job.token]);
        assert!(scheduler.take_ready().is_empty());
        assert!(scheduler
            .finish(
                job.token,
                Content::Text {
                    body: "stale".into(),
                    truncated: false
                }
            )
            .is_none());
        assert_eq!(scheduler.take_ready().len(), 1);
    }
    #[test]
    fn queue_and_cache_are_bounded() {
        let mut scheduler = PreviewScheduler::new(SchedulerSettings {
            max_queued: 1,
            max_cache_bytes: 270,
            max_cache_entries: 1,
            ..Default::default()
        })
        .unwrap();
        let request = |revision| PreviewRequest {
            id: revision,
            key: key(revision),
            name: "a".into(),
            max_input_bytes: 1024,
            max_text_lines: 10,
            max_output_pixels: 10,
        };
        scheduler.enqueue(request(1), Settings::default()).unwrap();
        assert_eq!(
            scheduler
                .enqueue(request(2), Settings::default())
                .unwrap_err(),
            SchedulerError::QueueFull
        );
        let job = scheduler.take_ready().pop().unwrap();
        scheduler
            .finish(
                job.token,
                Content::Text {
                    body: "one".into(),
                    truncated: false,
                },
            )
            .unwrap();
        scheduler.enqueue(request(2), Settings::default()).unwrap();
        let job = scheduler.take_ready().pop().unwrap();
        scheduler
            .finish(
                job.token,
                Content::Text {
                    body: "two".into(),
                    truncated: false,
                },
            )
            .unwrap();
        assert_eq!(scheduler.diagnostics().cached, 1);
        assert!(scheduler.diagnostics().cache_bytes <= 270);
        assert!(matches!(
            scheduler
                .enqueue(request(1), Settings::default())
                .unwrap()
                .0,
            Submission::Queued { .. }
        ));
    }
    #[test]
    fn workspace_switch_drops_cache_but_pointer_leave_preserves_it() {
        let mut scheduler = PreviewScheduler::default();
        let request = PreviewRequest {
            id: 1,
            key: key(1),
            name: "a".into(),
            max_input_bytes: 1024,
            max_text_lines: 10,
            max_output_pixels: 10,
        };
        scheduler
            .enqueue(request.clone(), Settings::default())
            .unwrap();
        let job = scheduler.take_ready().pop().unwrap();
        scheduler
            .finish(
                job.token,
                Content::Text {
                    body: "private".into(),
                    truncated: false,
                },
            )
            .unwrap();
        scheduler.retain_key(None);
        assert_eq!(scheduler.diagnostics().cached, 1);
        let mut other = key(1);
        other.scope = "other-account".into();
        scheduler.retain_key(Some(&other));
        assert_eq!(scheduler.diagnostics().cache_bytes, 0);
        assert_eq!(scheduler.diagnostics().cached, 0);
        assert!(matches!(
            scheduler.enqueue(request, Settings::default()).unwrap().0,
            Submission::Queued { .. }
        ));
    }
    #[test]
    fn disabling_peak_cancels_owned_worker_without_replaying_it() {
        let mut scheduler = PreviewScheduler::default();
        let mut peak = Peak::default();
        peak.hover(key(1), "a", vec![], 0);
        peak.tick_scheduled(&mut scheduler, 1650).unwrap();
        let job = scheduler.take_ready().pop().unwrap();
        peak.settings.enabled = false;
        assert_eq!(peak.tick_scheduled(&mut scheduler, 1700).unwrap(), None);
        assert_eq!(scheduler.take_cancellations(), vec![job.token]);
        assert_eq!(scheduler.diagnostics().active, 1);
        peak.settings.enabled = true;
        assert!(scheduler
            .finish(
                job.token,
                Content::Text {
                    body: "stale".into(),
                    truncated: false
                }
            )
            .is_none());
        assert_eq!(scheduler.diagnostics().cache_bytes, 0);
        assert!(peak.content().is_none());
    }
}
