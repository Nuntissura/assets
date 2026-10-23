use std::collections::BTreeSet;
use serde::{Deserialize, Serialize};
use rss_contracts::{Error, HostRef, OperationId, Result, SourceRequest};
use crate::{invalid, validate_request};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobLimits {
    pub max_jobs: usize,
    pub max_running: usize,
    pub max_attempts: u32,
    pub timeout_ticks: u64,
    pub initial_backoff_ticks: u64,
    pub max_backoff_ticks: u64,
}
impl JobLimits {
    fn validate(&self) -> Result<()> {
        if self.max_jobs == 0 || self.max_running == 0 || self.max_running > self.max_jobs || self.max_attempts == 0
            || self.timeout_ticks == 0 || self.initial_backoff_ticks == 0 || self.max_backoff_ticks < self.initial_backoff_ticks {
            return Err(invalid("job_limits"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum JobState {
    Queued { eligible_at: u64 }, Running { deadline: u64 }, Backoff { eligible_at: u64 },
    AwaitingRecovery, Reconnect, Failed, Cancelled, Completed,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Job {
    pub request: SourceRequest,
    pub attempts: u32,
    pub cancel_requested: bool,
    pub state: JobState,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobQueue { limits: JobLimits, jobs: Vec<Job> }
fn reserves_capacity(job: &Job) -> bool { matches!(job.state, JobState::Running { .. } | JobState::AwaitingRecovery) }

impl JobQueue {
    pub fn new(limits: JobLimits) -> Result<Self> { limits.validate()?; Ok(Self { limits, jobs: vec![] }) }
    pub fn jobs(&self) -> &[Job] { &self.jobs }
    pub fn validate(&self) -> Result<()> {
        self.limits.validate()?;
        if self.jobs.len() > self.limits.max_jobs { return Err(Error::LimitExceeded { field: "jobs".into() }); }
        let mut subscriptions = BTreeSet::new();
        let mut operations = BTreeSet::new();
        for job in &self.jobs {
            validate_request(&job.request)?;
            if job.attempts > self.limits.max_attempts || !operations.insert(job.request.operation_id)
                || !subscriptions.insert(&job.request.subscription.subscription_ref) { return Err(invalid("jobs.identity")); }
            if job.attempts == 0 && matches!(job.state, JobState::Running { .. } | JobState::Backoff { .. }
                | JobState::AwaitingRecovery | JobState::Completed) { return Err(invalid("jobs.attempts")); }
        }
        if self.jobs.iter().filter(|job| reserves_capacity(job)).count() > self.limits.max_running {
            return Err(Error::LimitExceeded { field: "running_jobs".into() });
        }
        Ok(())
    }
    pub fn enqueue(&mut self, request: SourceRequest, now: u64) -> Result<()> {
        self.validate()?; validate_request(&request)?;
        if let Some(existing) = self.jobs.iter().find(|job| job.request.operation_id == request.operation_id) {
            return if existing.request == request { Ok(()) } else { Err(Error::Conflict) };
        }
        if self.jobs.iter().any(|job| job.request.subscription.subscription_ref == request.subscription.subscription_ref) { return Err(Error::Conflict); }
        if self.jobs.len() == self.limits.max_jobs { return Err(Error::LimitExceeded { field: "jobs".into() }); }
        self.jobs.push(Job { request, attempts: 0, cancel_requested: false, state: JobState::Queued { eligible_at: now } });
        Ok(())
    }
    pub fn start(&mut self, operation: OperationId, executor: &HostRef, now: u64) -> Result<SourceRequest> {
        self.validate()?;
        let index = self.jobs.iter().position(|job| job.request.operation_id == operation).ok_or(Error::NotFound)?;
        let job = &self.jobs[index];
        if &job.request.subscription.executor_ref != executor { return Err(Error::Denied); }
        if job.cancel_requested || job.request.subscription.paused { return Err(Error::Cancelled); }
        let eligible_at = match job.state { JobState::Queued { eligible_at } | JobState::Backoff { eligible_at } => eligible_at, _ => return Err(Error::Conflict) };
        if now < eligible_at || job.attempts == self.limits.max_attempts { return Err(Error::Conflict); }
        if self.jobs.iter().filter(|job| reserves_capacity(job)).count() == self.limits.max_running {
            return Err(Error::LimitExceeded { field: "running_jobs".into() });
        }
        let deadline = now.checked_add(self.limits.timeout_ticks).ok_or_else(|| invalid("job.deadline"))?;
        let job = &mut self.jobs[index];
        job.attempts += 1; job.state = JobState::Running { deadline };
        Ok(job.request.clone())
    }
    /// Call before the first item-owner mutation; persist this state before executing the effect.
    pub fn begin_commit(&mut self, operation: OperationId, executor: &HostRef, now: u64) -> Result<()> {
        self.validate()?;
        let job = self.job_mut(operation)?;
        if &job.request.subscription.executor_ref != executor { return Err(Error::Denied); }
        if job.cancel_requested || job.request.subscription.paused { return Err(Error::Cancelled); }
        if !matches!(job.state, JobState::Running { deadline } if now < deadline) { return Err(Error::Conflict); }
        job.state = JobState::AwaitingRecovery; Ok(())
    }
    pub fn fail(&mut self, operation: OperationId, error: &Error, now: u64, retry_after: Option<u64>) -> Result<()> {
        self.validate()?;
        let limits = self.limits.clone();
        let job = self.job_mut(operation)?;
        if matches!(job.state, JobState::AwaitingRecovery) { return Err(Error::UnknownOutcome { operation_id: operation }); }
        if !matches!(job.state, JobState::Running { .. }) { return Err(Error::Conflict); }
        job.state = match error {
            Error::UnknownOutcome { .. } | Error::ItemOwner { error: hsscrap::Error::UnknownOutcome { .. } } => JobState::AwaitingRecovery,
            Error::ReconnectRequired | Error::Expired => JobState::Reconnect,
            Error::Cancelled => JobState::Cancelled,
            Error::Network | Error::RateLimited { .. } if job.attempts < limits.max_attempts => {
                let backoff = limits.initial_backoff_ticks.saturating_mul(2u64.saturating_pow(job.attempts - 1)).min(limits.max_backoff_ticks);
                let eligible_at = now.checked_add(backoff).ok_or_else(|| invalid("job.backoff"))?.max(retry_after.unwrap_or(0));
                JobState::Backoff { eligible_at }
            }
            _ => JobState::Failed,
        };
        Ok(())
    }
    pub fn cancel(&mut self, operation: OperationId) -> Result<()> {
        self.validate()?;
        let job = self.job_mut(operation)?;
        job.cancel_requested = true;
        if matches!(job.state, JobState::Queued { .. } | JobState::Backoff { .. } | JobState::Reconnect) { job.state = JobState::Cancelled; }
        Ok(())
    }
    /// A restart or elapsed running deadline does not establish that an external effect stopped.
    pub fn recover_interrupted(&mut self, now: u64, restarted: bool) -> Result<()> {
        self.validate()?;
        for job in &mut self.jobs {
            if matches!(job.state, JobState::Running { deadline } if restarted || now >= deadline) { job.state = JobState::AwaitingRecovery; }
        }
        Ok(())
    }
    pub fn remove_terminal(&mut self, operation: OperationId) -> Result<()> {
        self.validate()?;
        let index = self.jobs.iter().position(|job| job.request.operation_id == operation).ok_or(Error::NotFound)?;
        if !matches!(self.jobs[index].state, JobState::Completed | JobState::Cancelled | JobState::Failed | JobState::Reconnect) { return Err(Error::Conflict); }
        self.jobs.remove(index); Ok(())
    }
    fn job_mut(&mut self, operation: OperationId) -> Result<&mut Job> {
        self.jobs.iter_mut().find(|job| job.request.operation_id == operation).ok_or(Error::NotFound)
    }
}
