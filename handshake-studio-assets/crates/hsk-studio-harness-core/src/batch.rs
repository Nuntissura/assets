//! Batch runner over documents (lane SMB-PS-ACTIONS-1): the [STU-AUT-126] job record, its source
//! sets ([STU-AUT-127]), applied automation ([STU-AUT-128]), output targets ([STU-AUT-129]),
//! filename template ([STU-AUT-130]), overwrite and error policies ([STU-AUT-131]), per-file
//! receipts ([STU-AUT-132]), the portable job artifact ([STU-AUT-133]) and quiet-law conformance
//! ([STU-AUT-136]): no window, no prompt, structured job state, cancellation between inputs and
//! between steps. Tests `tests/action_batch.rs`.
//!
//! Every input runs in its own [`Run`] of the caller's [`Harness`]: open, digest, Job Runtime
//! admission (optional [`JobPort`], one ticket per input, released on every exit path), the
//! automation through [`Run::play_action`] (headless), then one native package per output target
//! through [`Run::save_to`]. Inputs are never moved, deleted or written over.
//!
//! Studio readings (UNVERIFIED (D5), no Photoshop oracle): `*`/`?` folder patterns match the file
//! name only (case-sensitive, symlinks not followed); concurrency is 1 (sequential); the date,
//! time, dimension and scale-suffix template tokens are refused (no clock port; refusing keeps runs
//! deterministic); `quarantine` COPIES the failing input into its folder (never moves it); retries
//! apply to failures before any output is written (an output-stage failure is never retried, so a
//! retry cannot write a second renamed output); the backoff is recorded, never slept; the
//! `JobScope` resource is the opened document's id, so admission follows the open.
#![deny(clippy::arithmetic_side_effects)]

use crate::action::{self, ActionRecord, ActionStep, DecodeError, DialogOption, Obj, PlayMode, parse_strict};
use crate::harness::{Harness, Run, default_actor};
use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};
use hsk_studio_session::{Action as JobAction, Cost, JobPort, JobScope, JobTicket};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

/// Most inputs of one job (the sequence-number soft maximum of [STU-AUT-130]).
pub const MAX_INPUTS: usize = 100_000;
/// Most output targets of one job.
pub const MAX_TARGETS: usize = 64;
/// Most filename-template tokens.
pub const MAX_TEMPLATE_TOKENS: usize = 64;
/// Most include or exclude patterns.
pub const MAX_PATTERNS: usize = 256;
/// Largest declared retry count of `retry_then_skip`.
pub const MAX_RETRIES: u64 = 16;
/// Largest input file read.
pub const MAX_INPUT_BYTES: u64 = 4_294_967_296;
/// Deepest folder recursion.
pub const MAX_FOLDER_DEPTH: usize = 32;
/// [STU-AUT-130] sequence number: hard_min 0, hard_max 2^31-1 (Studio's declaration), soft 1..=100000, default 1.
pub const SEQUENCE_HARD_MAX: u64 = 2_147_483_647;
pub const SEQUENCE_SOFT_MIN: u64 = 1;
pub const SEQUENCE_SOFT_MAX: u64 = 100_000;
pub const SEQUENCE_DEFAULT: u64 = 1;
/// Widest zero padding of the sequence number.
pub const MAX_SEQUENCE_PADDING: u64 = 10;
/// Most rename suffixes tried for one output.
const MAX_RENAME_SUFFIX: u32 = 9999;

/// [STU-AUT-127] source set: closed, five kinds; `explicit_file_list` and `folder` run, the other
/// three refuse at start (`batch_source_kind_unsupported`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SourceSet {
    ExplicitFileList(Vec<String>),
    Folder { folder: String, recurse: bool, include: Vec<String>, exclude: Vec<String> },
    OpenDocuments,
    SelectionWithinDocument { document: String, scope: String },
    CatalogQuery { query: String },
}

impl SourceSet {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::ExplicitFileList(_) => "explicit_file_list",
            Self::Folder { .. } => "folder",
            Self::OpenDocuments => "open_documents",
            Self::SelectionWithinDocument { .. } => "selection_within_document",
            Self::CatalogQuery { .. } => "catalog_query",
        }
    }

    fn to_json(&self) -> Value {
        match self {
            Self::ExplicitFileList(files) => json!({"kind": self.kind(), "files": files}),
            Self::Folder { folder, recurse, include, exclude } => json!({"kind": self.kind(), "folder": folder, "recurse": recurse, "include": include, "exclude": exclude}),
            Self::OpenDocuments => json!({"kind": self.kind()}),
            Self::SelectionWithinDocument { document, scope } => json!({"kind": self.kind(), "document": document, "scope": scope}),
            Self::CatalogQuery { query } => json!({"kind": self.kind(), "query": query}),
        }
    }

    fn from_json(v: &Value, path: &str) -> Result<Self, DecodeError> {
        let kind = Obj::new(v, path, &["kind", "files", "folder", "recurse", "include", "exclude", "document", "scope", "query"])?.text("kind")?;
        Ok(match kind.as_str() {
            "explicit_file_list" => Self::ExplicitFileList(texts(&Obj::new(v, path, &["kind", "files"])?, "files", MAX_INPUTS)?),
            "folder" => {
                let o = Obj::new(v, path, &["kind", "folder", "recurse", "include", "exclude"])?;
                Self::Folder { folder: o.text("folder")?, recurse: o.boolean("recurse")?, include: texts(&o, "include", MAX_PATTERNS)?, exclude: texts(&o, "exclude", MAX_PATTERNS)? }
            }
            "open_documents" => {
                Obj::new(v, path, &["kind"])?;
                Self::OpenDocuments
            }
            "selection_within_document" => {
                let o = Obj::new(v, path, &["kind", "document", "scope"])?;
                let scope = o.text("scope")?;
                if !matches!(scope.as_str(), "artboard" | "layer") {
                    return Err(DecodeError::new("source_scope_out_of_domain", format!("{path}/scope")));
                }
                Self::SelectionWithinDocument { document: o.text("document")?, scope }
            }
            "catalog_query" => Self::CatalogQuery { query: Obj::new(v, path, &["kind", "query"])?.text("query")? },
            _ => return Err(DecodeError::new("source_kind_unknown", format!("{path}/kind"))),
        })
    }
}

fn texts(o: &Obj<'_>, k: &str, max: usize) -> Result<Vec<String>, DecodeError> {
    let a = o.array(k, max)?;
    a.iter()
        .enumerate()
        .map(|(i, v)| {
            v.as_str()
                .filter(|s| s.len() <= action::MAX_TEXT_BYTES)
                .map(str::to_string)
                .ok_or_else(|| DecodeError::new("field_type", format!("/{k}/{i}")))
        })
        .collect()
}

/// [STU-AUT-128] applied automation: closed, three kinds. `action` carries the action record inline
/// (portable; an action library to resolve ids against does not exist yet: Folio persistence gap).
#[derive(Clone, Debug, PartialEq)]
pub enum Automation {
    Action(ActionRecord),
    CommandSequence(Vec<ActionStep>),
    None,
}

impl Automation {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Action(_) => "action",
            Self::CommandSequence(_) => "command_sequence",
            Self::None => "none",
        }
    }

    fn to_json(&self) -> Value {
        match self {
            Self::Action(a) => json!({"kind": self.kind(), "action": a.to_json()}),
            Self::CommandSequence(steps) => json!({"kind": self.kind(), "steps": steps.iter().map(ActionStep::to_json).collect::<Vec<_>>()}),
            Self::None => json!({"kind": self.kind()}),
        }
    }

    fn from_json(v: &Value, path: &str) -> Result<Self, DecodeError> {
        let kind = Obj::new(v, path, &["kind", "action", "steps"])?.text("kind")?;
        Ok(match kind.as_str() {
            "action" => {
                let o = Obj::new(v, path, &["kind", "action"])?;
                let mut budget = action::MAX_STEPS_PER_SET;
                Self::Action(ActionRecord::from_json(o.get("action")?, &format!("{path}/action"), &mut budget)?)
            }
            "command_sequence" => {
                let o = Obj::new(v, path, &["kind", "steps"])?;
                let raw = o.array("steps", action::MAX_STEPS_PER_ACTION)?;
                let mut steps = Vec::with_capacity(raw.len());
                for (i, s) in raw.iter().enumerate() {
                    steps.push(ActionStep::from_json(s, &format!("{path}/steps/{i}"))?);
                }
                Self::CommandSequence(steps)
            }
            "none" => {
                Obj::new(v, path, &["kind"])?;
                Self::None
            }
            _ => return Err(DecodeError::new("automation_kind_unknown", format!("{path}/kind"))),
        })
    }

    /// The steps the automation plays (empty for `none`).
    pub fn steps(&self) -> &[ActionStep] {
        match self {
            Self::Action(a) => &a.steps,
            Self::CommandSequence(s) => s,
            Self::None => &[],
        }
    }

    fn owner_id(&self, job_id: &str) -> String {
        match self {
            Self::Action(a) => a.action_id.clone(),
            _ => format!("{job_id}/command_sequence"),
        }
    }
}

/// [STU-AUT-131] overwrite policy: closed, four members (default `rename` for headless jobs).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OverwritePolicy {
    /// Interactive only: a headless job refuses to start (`batch_ask_overwrite_in_headless`).
    Ask,
    Overwrite,
    #[default]
    Rename,
    Skip,
}

impl OverwritePolicy {
    pub const ALL: [OverwritePolicy; 4] = [Self::Ask, Self::Overwrite, Self::Rename, Self::Skip];

    pub const fn token(self) -> &'static str {
        match self {
            Self::Ask => "ask",
            Self::Overwrite => "overwrite",
            Self::Rename => "rename",
            Self::Skip => "skip",
        }
    }
}

/// [STU-AUT-129] output target. Only `format_id` `handshake` (native package through
/// [`Run::save_to`]) is written; any other format, an export recipe and a non-null colour-profile or
/// resize policy refuse that target with a typed code.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputTarget {
    pub format_id: String,
    pub export_recipe_id: Option<String>,
    /// The output folder.
    pub destination: String,
    pub colour_profile_policy: Option<String>,
    pub resize_policy: Option<String>,
    pub overwrite_policy: OverwritePolicy,
}

impl OutputTarget {
    const FIELDS: [&'static str; 6] = ["format_id", "export_recipe_id", "destination", "colour_profile_policy", "resize_policy", "overwrite_policy"];

    fn to_json(&self) -> Value {
        json!({
            "format_id": self.format_id, "export_recipe_id": self.export_recipe_id, "destination": self.destination,
            "colour_profile_policy": self.colour_profile_policy, "resize_policy": self.resize_policy, "overwrite_policy": self.overwrite_policy.token(),
        })
    }

    fn from_json(v: &Value, path: &str) -> Result<Self, DecodeError> {
        let o = Obj::new(v, path, &Self::FIELDS)?;
        let policy = o.text("overwrite_policy")?;
        let overwrite_policy = OverwritePolicy::ALL.into_iter().find(|p| p.token() == policy).ok_or_else(|| DecodeError::new("overwrite_policy_out_of_domain", format!("{path}/overwrite_policy")))?;
        Ok(Self {
            format_id: o.text("format_id")?,
            export_recipe_id: o.opt_text("export_recipe_id")?,
            destination: o.text("destination")?,
            colour_profile_policy: o.opt_text("colour_profile_policy")?,
            resize_policy: o.opt_text("resize_policy")?,
            overwrite_policy,
        })
    }
}

/// [STU-AUT-130] filename-template tokens (a structured token list, Studio reading of "a token
/// string"). The output file name is the rendered template plus the format's extension.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TemplateToken {
    OriginalFilename,
    OriginalExtension,
    /// Sequence number: `start` (hard 0..=2^31-1, soft 1..=100000, default 1) plus the input's
    /// position, zero-padded to `padding` digits (0..=10).
    Sequence { start: u64, padding: u64 },
    OutputFormat,
    Custom(String),
    /// Refused at start (`batch_template_token_unsupported`): no clock port.
    Date,
    Time,
    /// Refused at start: document dimensions are not part of this runner.
    DocumentWidth,
    DocumentHeight,
    /// Refused at start: no scale step in this runner.
    ScaleSuffix,
}

impl TemplateToken {
    pub fn token(&self) -> &'static str {
        match self {
            Self::OriginalFilename => "original_filename",
            Self::OriginalExtension => "original_extension",
            Self::Sequence { .. } => "sequence",
            Self::OutputFormat => "output_format",
            Self::Custom(_) => "custom",
            Self::Date => "date",
            Self::Time => "time",
            Self::DocumentWidth => "document_width",
            Self::DocumentHeight => "document_height",
            Self::ScaleSuffix => "scale_suffix",
        }
    }

    fn to_json(&self) -> Value {
        match self {
            Self::Sequence { start, padding } => json!({"token": self.token(), "start": start, "padding": padding}),
            Self::Custom(text) => json!({"token": self.token(), "text": text}),
            _ => json!({"token": self.token()}),
        }
    }

    fn from_json(v: &Value, path: &str) -> Result<Self, DecodeError> {
        let token = Obj::new(v, path, &["token", "start", "padding", "text"])?.text("token")?;
        let bare = |t: Self| Obj::new(v, path, &["token"]).map(|_| t);
        match token.as_str() {
            "sequence" => {
                let o = Obj::new(v, path, &["token", "start", "padding"])?;
                Ok(Self::Sequence { start: o.uint("start")?, padding: o.uint("padding")? })
            }
            "custom" => Ok(Self::Custom(Obj::new(v, path, &["token", "text"])?.text("text")?)),
            "original_filename" => bare(Self::OriginalFilename),
            "original_extension" => bare(Self::OriginalExtension),
            "output_format" => bare(Self::OutputFormat),
            "date" => bare(Self::Date),
            "time" => bare(Self::Time),
            "document_width" => bare(Self::DocumentWidth),
            "document_height" => bare(Self::DocumentHeight),
            "scale_suffix" => bare(Self::ScaleSuffix),
            _ => Err(DecodeError::new("template_token_unknown", format!("{path}/token"))),
        }
    }
}

/// [STU-AUT-131] error policy: closed, four members (default `skip_and_continue`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum ErrorPolicy {
    StopOnFirstError,
    #[default]
    SkipAndContinue,
    /// Retries a failure before any output is written `retry_count` times (0..=16); the backoff is
    /// recorded in the receipt, never slept.
    RetryThenSkip { retry_count: u64, backoff_ms: u64 },
    /// Copies (never moves) the failing input into `folder`, then continues.
    Quarantine { folder: String },
}

impl ErrorPolicy {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::StopOnFirstError => "stop_on_first_error",
            Self::SkipAndContinue => "skip_and_continue",
            Self::RetryThenSkip { .. } => "retry_then_skip",
            Self::Quarantine { .. } => "quarantine",
        }
    }

    fn to_json(&self) -> Value {
        match self {
            Self::RetryThenSkip { retry_count, backoff_ms } => json!({"kind": self.kind(), "retry_count": retry_count, "backoff_ms": backoff_ms}),
            Self::Quarantine { folder } => json!({"kind": self.kind(), "folder": folder}),
            _ => json!({"kind": self.kind()}),
        }
    }

    fn from_json(v: &Value, path: &str) -> Result<Self, DecodeError> {
        let kind = Obj::new(v, path, &["kind", "retry_count", "backoff_ms", "folder"])?.text("kind")?;
        match kind.as_str() {
            "stop_on_first_error" => Obj::new(v, path, &["kind"]).map(|_| Self::StopOnFirstError),
            "skip_and_continue" => Obj::new(v, path, &["kind"]).map(|_| Self::SkipAndContinue),
            "retry_then_skip" => {
                let o = Obj::new(v, path, &["kind", "retry_count", "backoff_ms"])?;
                Ok(Self::RetryThenSkip { retry_count: o.uint("retry_count")?, backoff_ms: o.uint("backoff_ms")? })
            }
            "quarantine" => Ok(Self::Quarantine { folder: Obj::new(v, path, &["kind", "folder"])?.text("folder")? }),
            _ => Err(DecodeError::new("error_policy_out_of_domain", format!("{path}/kind"))),
        }
    }
}

/// [STU-AUT-126] batch job record; [`BatchJob::export`] / [`BatchJob::decode`] are the portable,
/// re-runnable artifact ([STU-AUT-133]).
#[derive(Clone, Debug, PartialEq)]
pub struct BatchJob {
    pub job_id: String,
    pub source_set: SourceSet,
    pub applied_automation: Automation,
    pub output_targets: Vec<OutputTarget>,
    pub filename_template: Vec<TemplateToken>,
    pub error_policy: ErrorPolicy,
    /// Only 1 runs (sequential, Studio reading); other values refuse `batch_concurrency_unsupported`.
    pub concurrency: u64,
    /// Not supported: a non-empty list refuses `batch_post_processing_unsupported`.
    pub post_processing: Vec<String>,
}

impl BatchJob {
    const FIELDS: [&'static str; 8] = ["job_id", "source_set", "applied_automation", "output_targets", "filename_template", "error_policy", "concurrency", "post_processing"];

    pub fn to_json(&self) -> Value {
        json!({
            "job_id": self.job_id, "source_set": self.source_set.to_json(), "applied_automation": self.applied_automation.to_json(),
            "output_targets": self.output_targets.iter().map(OutputTarget::to_json).collect::<Vec<_>>(),
            "filename_template": self.filename_template.iter().map(TemplateToken::to_json).collect::<Vec<_>>(),
            "error_policy": self.error_policy.to_json(), "concurrency": self.concurrency, "post_processing": self.post_processing,
        })
    }

    /// The portable job artifact (compact JSON of [`BatchJob::to_json`]).
    pub fn export(&self) -> Vec<u8> {
        serde_json::to_vec(&self.to_json()).unwrap_or_default()
    }

    /// Strict decode of a job artifact (unknown, duplicate or missing fields are typed refusals).
    pub fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        let v = parse_strict(bytes)?;
        let o = Obj::new(&v, "", &Self::FIELDS)?;
        let job_id = o.text("job_id")?;
        if job_id.is_empty() {
            return Err(DecodeError::new("field_type", "/job_id"));
        }
        let targets = o.array("output_targets", MAX_TARGETS)?;
        let template = o.array("filename_template", MAX_TEMPLATE_TOKENS)?;
        Ok(Self {
            job_id,
            source_set: SourceSet::from_json(o.get("source_set")?, "/source_set")?,
            applied_automation: Automation::from_json(o.get("applied_automation")?, "/applied_automation")?,
            output_targets: targets.iter().enumerate().map(|(i, t)| OutputTarget::from_json(t, &format!("/output_targets/{i}"))).collect::<Result<_, _>>()?,
            filename_template: template.iter().enumerate().map(|(i, t)| TemplateToken::from_json(t, &format!("/filename_template/{i}"))).collect::<Result<_, _>>()?,
            error_policy: ErrorPolicy::from_json(o.get("error_policy")?, "/error_policy")?,
            concurrency: o.uint("concurrency")?,
            post_processing: texts(&o, "post_processing", MAX_PATTERNS)?,
        })
    }
}

// ----- running ---------------------------------------------------------------------------------

/// Structured job state ([STU-AUT-136]): progress, current input, outcomes and cancellation.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct JobState {
    pub done: usize,
    pub total: usize,
    pub current_input: Option<String>,
    pub succeeded: usize,
    pub failed: usize,
    pub cancelled: bool,
}

impl JobState {
    pub fn to_json(&self) -> Value {
        json!({"done": self.done, "total": self.total, "current_input": self.current_input, "succeeded": self.succeeded, "failed": self.failed, "cancelled": self.cancelled})
    }
}

/// What the runner is given besides the job: the optional Job Runtime port, the attribution, and an
/// optional state callback (called at start, at each input start and after each input).
#[derive(Default)]
pub struct BatchPorts<'a> {
    pub jobs: Option<Arc<dyn JobPort>>,
    pub actor: Option<ActorContext>,
    pub on_state: Option<&'a mut dyn FnMut(&JobState)>,
}

/// A job that refused to start: nothing was read or written.
#[derive(Clone, Debug, PartialEq)]
pub struct BatchRefusal {
    pub code: &'static str,
    pub detail: Value,
}

impl BatchRefusal {
    fn new(code: &'static str, detail: Value) -> Self {
        Self { code, detail }
    }

    pub fn to_json(&self) -> Value {
        json!({"code": self.code, "detail": self.detail})
    }
}

/// One output target's result for one input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TargetReceipt {
    pub format_id: String,
    /// The output path (the artifact identity; no ArtifactService manifest id yet: gap).
    pub path: Option<String>,
    /// `written`, `skipped_existing`, `refused` or `failed`.
    pub outcome: &'static str,
    pub code: Option<String>,
}

impl TargetReceipt {
    fn to_json(&self) -> Value {
        json!({"format_id": self.format_id, "path": self.path, "outcome": self.outcome, "code": self.code})
    }
}

/// [STU-AUT-132] per-input receipt. `elapsed_ms` is non-deterministic and left out of
/// [`InputReceipt::deterministic_json`].
#[derive(Clone, Debug, PartialEq)]
pub struct InputReceipt {
    pub input_path: String,
    pub input_bytes: Option<u64>,
    /// `{algorithm: sha256, digest}` of the input bytes (Session::stage_tile on the opened input:
    /// no input that fails to open has a digest; `digest_note` says why).
    pub content_digest: Option<Value>,
    pub digest_note: Option<String>,
    pub automation: Value,
    pub targets: Vec<TargetReceipt>,
    /// `succeeded`, `failed` or `cancelled`.
    pub outcome: &'static str,
    /// `{stage, code}` of the failure.
    pub error: Option<Value>,
    pub attempts: u64,
    pub retries: Vec<Value>,
    pub quarantined_to: Option<String>,
    pub elapsed_ms: u64,
}

impl InputReceipt {
    pub fn deterministic_json(&self) -> Value {
        json!({
            "input": {"path": self.input_path, "bytes": self.input_bytes, "content_digest": self.content_digest, "digest_note": self.digest_note},
            "automation": self.automation,
            "targets": self.targets.iter().map(TargetReceipt::to_json).collect::<Vec<_>>(),
            "outcome": self.outcome, "error": self.error, "attempts": self.attempts, "retries": self.retries,
            "quarantined_to": self.quarantined_to,
        })
    }

    pub fn to_json(&self) -> Value {
        let mut v = self.deterministic_json();
        v["elapsed_ms"] = json!(self.elapsed_ms);
        v["elapsed_deterministic"] = json!(false);
        v
    }
}

/// The job's aggregate result: the receipt set ([STU-AUT-132]) and the final state.
#[derive(Clone, Debug, PartialEq)]
pub struct BatchReport {
    pub job_id: String,
    pub receipts: Vec<InputReceipt>,
    pub state: JobState,
    /// Why the job stopped early: `stop_on_first_error` or `cancelled`.
    pub stopped: Option<&'static str>,
    /// Non-fatal notes (sequence start outside its soft range, skipped non-UTF-8 names).
    pub warnings: Vec<Value>,
}

impl BatchReport {
    pub fn to_json(&self) -> Value {
        json!({
            "job_id": self.job_id, "receipts": self.receipts.iter().map(InputReceipt::to_json).collect::<Vec<_>>(),
            "state": self.state.to_json(), "stopped": self.stopped, "warnings": self.warnings,
        })
    }
}

/// Checks the job before anything is read or written ([STU-AUT-136] start-time refusals:
/// `batch_concurrency_unsupported`, `batch_source_kind_unsupported`, `batch_ask_overwrite_in_headless`
/// ([STU-AUT-187], naming the job), `batch_always_show_dialog_in_headless` ([STU-AUT-186], naming
/// the step and its action), unplayable steps, unsupported template tokens and post-processing).
/// Returns the warnings.
pub fn check_start(job: &BatchJob) -> Result<Vec<Value>, BatchRefusal> {
    if job.concurrency != 1 {
        return Err(BatchRefusal::new("batch_concurrency_unsupported", json!({"job_id": job.job_id, "concurrency": job.concurrency})));
    }
    if !matches!(job.source_set, SourceSet::ExplicitFileList(_) | SourceSet::Folder { .. }) {
        return Err(BatchRefusal::new("batch_source_kind_unsupported", json!({"job_id": job.job_id, "kind": job.source_set.kind()})));
    }
    if job.output_targets.is_empty() {
        return Err(BatchRefusal::new("batch_no_output_targets", json!({"job_id": job.job_id})));
    }
    if job.output_targets.iter().any(|t| t.overwrite_policy == OverwritePolicy::Ask) {
        return Err(BatchRefusal::new("batch_ask_overwrite_in_headless", json!({"job_id": job.job_id})));
    }
    let owner = job.applied_automation.owner_id(&job.job_id);
    let steps = job.applied_automation.steps();
    if let Some(s) = steps.iter().find(|s| s.enabled && s.dialog_options == DialogOption::AlwaysShow) {
        return Err(BatchRefusal::new(
            "batch_always_show_dialog_in_headless",
            json!({"job_id": job.job_id, "action_id": owner, "step_index": s.index, "command_id": s.command_id}),
        ));
    }
    let mut diagnostics = match &job.applied_automation {
        Automation::Action(a) => action::validate_action(a),
        _ => {
            let mut d = Vec::new();
            action::validate_steps(&owner, steps, &mut d);
            d
        }
    };
    diagnostics.retain(|d| d.severity == action::Severity::Error);
    if let Some(d) = diagnostics.first() {
        return Err(BatchRefusal::new(d.code, d.fields.clone()));
    }
    for s in steps.iter().filter(|s| s.enabled) {
        if let Err(fault) = action::call_of(s) {
            return Err(BatchRefusal::new(fault.code, json!({"action_id": owner, "step_index": s.index, "command_id": s.command_id, "key": fault.key})));
        }
    }
    if job.filename_template.is_empty() {
        return Err(BatchRefusal::new("batch_template_empty", json!({"job_id": job.job_id})));
    }
    let mut warnings = Vec::new();
    for t in &job.filename_template {
        match t {
            TemplateToken::Date | TemplateToken::Time | TemplateToken::DocumentWidth | TemplateToken::DocumentHeight | TemplateToken::ScaleSuffix => {
                return Err(BatchRefusal::new("batch_template_token_unsupported", json!({"job_id": job.job_id, "token": t.token()})));
            }
            TemplateToken::Sequence { start, padding } => {
                if *start > SEQUENCE_HARD_MAX || *padding > MAX_SEQUENCE_PADDING {
                    return Err(BatchRefusal::new("batch_sequence_out_of_domain", json!({"job_id": job.job_id, "start": start, "padding": padding})));
                }
                if !(SEQUENCE_SOFT_MIN..=SEQUENCE_SOFT_MAX).contains(start) {
                    warnings.push(json!({"code": "batch_sequence_start_outside_soft_range", "start": start}));
                }
            }
            TemplateToken::Custom(text) if !name_part_ok(text) => {
                return Err(BatchRefusal::new("batch_template_text_invalid", json!({"job_id": job.job_id})));
            }
            _ => {}
        }
    }
    if !job.post_processing.is_empty() {
        return Err(BatchRefusal::new("batch_post_processing_unsupported", json!({"job_id": job.job_id})));
    }
    if let ErrorPolicy::RetryThenSkip { retry_count, .. } = job.error_policy
        && retry_count > MAX_RETRIES
    {
        return Err(BatchRefusal::new("batch_retry_count_out_of_domain", json!({"job_id": job.job_id, "retry_count": retry_count})));
    }
    Ok(warnings)
}

/// No path separator, drive colon, control character or parent reference.
fn name_part_ok(s: &str) -> bool {
    !s.contains("..") && !s.chars().any(|c| matches!(c, '/' | '\\' | ':') || c.is_control())
}

/// `*` (any run) and `?` (one character) over a file name; everything else matches literally.
pub fn pattern_matches(pattern: &str, name: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let n: Vec<char> = name.chars().collect();
    let (mut pi, mut ni) = (0usize, 0usize);
    let mut star: Option<(usize, usize)> = None;
    while ni < n.len() {
        match p.get(pi) {
            Some('*') => {
                star = Some((pi, ni));
                pi = pi.saturating_add(1);
            }
            Some(c) if *c == '?' || Some(c) == n.get(ni) => {
                pi = pi.saturating_add(1);
                ni = ni.saturating_add(1);
            }
            _ => match star {
                Some((sp, sn)) => {
                    pi = sp.saturating_add(1);
                    ni = sn.saturating_add(1);
                    star = Some((sp, ni));
                }
                None => return false,
            },
        }
    }
    p.get(pi..).is_some_and(|rest| rest.iter().all(|c| *c == '*'))
}

/// The inputs of the job in run order (folders sorted by path).
fn enumerate(job: &BatchJob, warnings: &mut Vec<Value>) -> Result<Vec<PathBuf>, BatchRefusal> {
    let inputs = match &job.source_set {
        SourceSet::ExplicitFileList(files) => files.iter().map(PathBuf::from).collect(),
        SourceSet::Folder { folder, recurse, include, exclude } => {
            let mut out = Vec::new();
            walk(Path::new(folder), *recurse, 0, include, exclude, &mut out, warnings).map_err(|_| BatchRefusal::new("batch_source_unreadable", json!({"job_id": job.job_id, "folder": folder})))?;
            out.sort();
            out
        }
        _ => return Err(BatchRefusal::new("batch_source_kind_unsupported", json!({"job_id": job.job_id, "kind": job.source_set.kind()}))),
    };
    if inputs.len() > MAX_INPUTS {
        return Err(BatchRefusal::new("batch_too_many_inputs", json!({"job_id": job.job_id, "inputs": inputs.len()})));
    }
    Ok(inputs)
}

fn walk(dir: &Path, recurse: bool, depth: usize, include: &[String], exclude: &[String], out: &mut Vec<PathBuf>, warnings: &mut Vec<Value>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_dir() {
            if recurse && depth < MAX_FOLDER_DEPTH {
                walk(&entry.path(), recurse, depth.saturating_add(1), include, exclude, out, warnings)?;
            }
            continue;
        }
        if !kind.is_file() || out.len() > MAX_INPUTS {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            warnings.push(json!({"code": "batch_input_name_not_utf8", "path": entry.path().to_string_lossy()}));
            continue;
        };
        let included = include.is_empty() || include.iter().any(|p| pattern_matches(p, &name));
        if included && !exclude.iter().any(|p| pattern_matches(p, &name)) {
            out.push(entry.path());
        }
    }
    Ok(())
}

/// The output file name of one input for one target.
fn render_name(template: &[TemplateToken], input: &Path, position: usize, format_id: &str) -> Result<String, &'static str> {
    let stem = input.file_stem().and_then(|s| s.to_str()).unwrap_or_default();
    let ext = input.extension().and_then(|s| s.to_str()).unwrap_or_default();
    let mut name = String::new();
    for t in template {
        match t {
            TemplateToken::OriginalFilename => name.push_str(stem),
            TemplateToken::OriginalExtension => name.push_str(ext),
            TemplateToken::OutputFormat => name.push_str(format_id),
            TemplateToken::Custom(text) => name.push_str(text),
            TemplateToken::Sequence { start, padding } => {
                let n = u64::try_from(position).ok().and_then(|p| start.checked_add(p)).filter(|n| *n <= SEQUENCE_HARD_MAX).ok_or("batch_sequence_overflow")?;
                let width = usize::try_from(*padding).unwrap_or(0);
                name.push_str(&format!("{n:0width$}"));
            }
            _ => return Err("batch_template_token_unsupported"),
        }
    }
    if name.is_empty() || name == "." || !name_part_ok(&name) {
        return Err("batch_output_name_invalid");
    }
    Ok(name)
}

/// A Job Runtime ticket, released on drop (every exit path).
struct Ticket<'p> {
    port: &'p Arc<dyn JobPort>,
    ticket: Option<JobTicket>,
}

impl Drop for Ticket<'_> {
    fn drop(&mut self) {
        if let Some(t) = self.ticket.take() {
            self.port.release(t);
        }
    }
}

/// A failure of one attempt: stage, code, and whether an output was already written.
struct Failure {
    stage: &'static str,
    code: String,
    retryable: bool,
}

impl Failure {
    fn before_output(stage: &'static str, code: impl Into<String>) -> Self {
        Self { stage, code: code.into(), retryable: true }
    }
}

/// What one attempt produced (also on failure: identity and targets so far).
#[derive(Default)]
struct Attempt {
    input_bytes: Option<u64>,
    content_digest: Option<Value>,
    digest_note: Option<String>,
    targets: Vec<TargetReceipt>,
}

/// Runs the job: start checks, then every input in order. `Err` only when the job refused to
/// start (nothing touched); per-input failures are receipts under the error policy.
pub fn run_batch(harness: &Harness, job: &BatchJob, ports: BatchPorts<'_>, cancel: &CancellationToken) -> Result<BatchReport, BatchRefusal> {
    let mut warnings = check_start(job)?;
    let inputs = enumerate(job, &mut warnings)?;
    let BatchPorts { jobs, actor, mut on_state } = ports;
    let actor = actor.unwrap_or_else(default_actor);
    let mut state = JobState { total: inputs.len(), ..JobState::default() };
    let mut emit = |s: &JobState| {
        if let Some(cb) = on_state.as_mut() {
            cb(s);
        }
    };
    emit(&state);
    let automation_json = automation_receipt(&job.applied_automation, &job.job_id);
    let mut written: BTreeSet<PathBuf> = BTreeSet::new();
    let mut receipts = Vec::with_capacity(inputs.len());
    let mut stopped = None;
    for (position, input) in inputs.iter().enumerate() {
        if cancel.check().is_err() {
            state.cancelled = true;
            stopped = Some("cancelled");
            break;
        }
        state.current_input = Some(input.to_string_lossy().into_owned());
        emit(&state);
        let started = Instant::now();
        let max_attempts = match job.error_policy {
            ErrorPolicy::RetryThenSkip { retry_count, .. } => retry_count.saturating_add(1),
            _ => 1,
        };
        let mut attempts = 0u64;
        let mut retries = Vec::new();
        let (attempt, failure) = loop {
            attempts = attempts.saturating_add(1);
            let mut attempt = Attempt::default();
            let result = process(harness, job, &actor, jobs.as_ref(), cancel, input, position, &mut written, &mut attempt);
            match result {
                Ok(()) => break (attempt, None),
                Err(f) if f.retryable && attempts < max_attempts && f.code != "canceled" => {
                    let backoff = match job.error_policy {
                        ErrorPolicy::RetryThenSkip { backoff_ms, .. } => backoff_ms,
                        _ => 0,
                    };
                    retries.push(json!({"attempt": attempts, "stage": f.stage, "code": f.code, "backoff_ms": backoff, "slept": false}));
                }
                Err(f) => break (attempt, Some(f)),
            }
        };
        let cancelled = failure.as_ref().is_some_and(|f| f.code == "canceled");
        let mut receipt = InputReceipt {
            input_path: input.to_string_lossy().into_owned(),
            input_bytes: attempt.input_bytes,
            content_digest: attempt.content_digest,
            digest_note: attempt.digest_note,
            automation: automation_json.clone(),
            targets: attempt.targets,
            outcome: if failure.is_none() {
                "succeeded"
            } else if cancelled {
                "cancelled"
            } else {
                "failed"
            },
            error: failure.as_ref().map(|f| json!({"stage": f.stage, "code": f.code})),
            attempts,
            retries,
            quarantined_to: None,
            elapsed_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        };
        state.done = state.done.saturating_add(1);
        if failure.is_none() {
            state.succeeded = state.succeeded.saturating_add(1);
        } else {
            state.failed = state.failed.saturating_add(1);
        }
        if failure.is_some()
            && !cancelled
            && let ErrorPolicy::Quarantine { folder } = &job.error_policy
        {
            receipt.quarantined_to = Some(quarantine(input, Path::new(folder)).unwrap_or_else(|code| format!("error:{code}")));
        }
        receipts.push(receipt);
        state.current_input = None;
        emit(&state);
        if cancelled {
            state.cancelled = true;
            stopped = Some("cancelled");
            break;
        }
        if failure.is_some() && job.error_policy == ErrorPolicy::StopOnFirstError {
            stopped = Some("stop_on_first_error");
            break;
        }
    }
    emit(&state);
    Ok(BatchReport { job_id: job.job_id.clone(), receipts, state, stopped, warnings })
}

fn automation_receipt(a: &Automation, job_id: &str) -> Value {
    let steps: Vec<Value> = a
        .steps()
        .iter()
        .map(|s| json!({"index": s.index, "enabled": s.enabled, "command_id": s.command_id, "parameters": s.to_json()["parameters"].clone()}))
        .collect();
    let mut v = json!({"kind": a.kind(), "steps": steps});
    if let Automation::Action(action) = a {
        v["action_id"] = json!(action.action_id);
        v["name"] = json!(action.name);
    } else if matches!(a, Automation::CommandSequence(_)) {
        v["action_id"] = json!(a.owner_id(job_id));
    }
    v
}

/// The last op record's error code of `run`.
fn last_error(run: &Run<'_>, fallback: &str) -> String {
    run.ops().last().and_then(|o| o["error"].as_str()).unwrap_or(fallback).to_string()
}

#[allow(clippy::too_many_arguments)]
fn process(
    harness: &Harness,
    job: &BatchJob,
    actor: &ActorContext,
    jobs: Option<&Arc<dyn JobPort>>,
    cancel: &CancellationToken,
    input: &Path,
    position: usize,
    written: &mut BTreeSet<PathBuf>,
    out: &mut Attempt,
) -> Result<(), Failure> {
    let meta = std::fs::metadata(input).map_err(|_| Failure::before_output("read", "input_unreadable"))?;
    if !meta.is_file() {
        return Err(Failure::before_output("read", "input_not_a_file"));
    }
    if meta.len() > MAX_INPUT_BYTES {
        return Err(Failure::before_output("read", "input_too_large"));
    }
    let bytes = std::fs::read(input).map_err(|_| Failure::before_output("read", "input_unreadable"))?;
    out.input_bytes = u64::try_from(bytes.len()).ok();
    out.digest_note = Some("input_not_opened".to_string());
    let mut run = harness.run(actor.clone(), cancel);
    if !run.open_bytes(&bytes) {
        return Err(Failure::before_output("open", last_error(&run, "open_failed")));
    }
    match run.stage_tile(bytes) {
        Ok(d) => {
            out.content_digest = Some(json!({"algorithm": d.algorithm, "digest": d.digest}));
            out.digest_note = None;
        }
        Err(code) => out.digest_note = Some(code.to_string()),
    }
    let _ticket = match jobs {
        None => None,
        Some(port) => {
            let doc = run.snapshot().map(|s| s.document().document_id.clone()).unwrap_or_default();
            let resource = DomainId::parse(&doc).map_err(|_| Failure::before_output("admission", "job_resource_invalid"))?;
            let cost = Cost { jobs: 1, bytes: out.input_bytes.unwrap_or(0) };
            match port.admit(&JobScope { resource: &resource, action: JobAction::Edit }, cost) {
                Ok(t) => Some(Ticket { port, ticket: Some(t) }),
                Err(refused) => return Err(Failure::before_output("admission", format!("job_admission_refused(retry_after_ms={})", refused.retry_after_ms))),
            }
        }
    };
    let played = match &job.applied_automation {
        Automation::None => true,
        Automation::Action(a) => run.play_action(a, PlayMode::Headless),
        Automation::CommandSequence(steps) => {
            let a = ActionRecord {
                action_id: job.applied_automation.owner_id(&job.job_id),
                index: 0,
                name: "command_sequence".to_string(),
                function_key_index: 0,
                shift_modifier: false,
                command_modifier: false,
                color_index: 0,
                expanded: false,
                declared_step_count: u64::try_from(steps.len()).unwrap_or(u64::MAX),
                steps: steps.clone(),
            };
            run.play_action(&a, PlayMode::Headless)
        }
    };
    if !played {
        return Err(Failure::before_output("automation", last_error(&run, "automation_failed")));
    }
    let mut first_failure: Option<Failure> = None;
    for target in &job.output_targets {
        let receipt = write_target(&mut run, job, target, input, position, written);
        if first_failure.is_none() && matches!(receipt.outcome, "refused" | "failed") {
            first_failure = Some(Failure { stage: "output", code: receipt.code.clone().unwrap_or_default(), retryable: false });
        }
        out.targets.push(receipt);
    }
    first_failure.map_or(Ok(()), Err)
}

fn write_target(run: &mut Run<'_>, job: &BatchJob, target: &OutputTarget, input: &Path, position: usize, written: &mut BTreeSet<PathBuf>) -> TargetReceipt {
    let receipt = |path: Option<&Path>, outcome: &'static str, code: Option<&str>| TargetReceipt {
        format_id: target.format_id.clone(),
        path: path.map(|p| p.to_string_lossy().into_owned()),
        outcome,
        code: code.map(str::to_string),
    };
    if target.format_id != "handshake" {
        return receipt(None, "refused", Some("batch_output_format_unsupported"));
    }
    if target.export_recipe_id.is_some() {
        return receipt(None, "refused", Some("batch_export_recipe_unsupported"));
    }
    if target.colour_profile_policy.is_some() || target.resize_policy.is_some() {
        return receipt(None, "refused", Some("batch_target_policy_unsupported"));
    }
    let name = match render_name(&job.filename_template, input, position, &target.format_id) {
        Ok(n) => n,
        Err(code) => return receipt(None, "refused", Some(code)),
    };
    let dir = Path::new(&target.destination);
    if std::fs::create_dir_all(dir).is_err() {
        return receipt(None, "failed", Some("batch_destination_unavailable"));
    }
    let mut path = dir.join(format!("{name}.handshake"));
    let taken = |p: &Path, written: &BTreeSet<PathBuf>| p.exists() || written.contains(p);
    if taken(&path, written) {
        match target.overwrite_policy {
            OverwritePolicy::Skip => return receipt(Some(&path), "skipped_existing", None),
            OverwritePolicy::Rename => {
                let free = (1..=MAX_RENAME_SUFFIX).map(|i| dir.join(format!("{name}-{i}.handshake"))).find(|p| !taken(p, written));
                match free {
                    Some(p) => path = p,
                    None => return receipt(Some(&path), "failed", Some("batch_rename_exhausted")),
                }
            }
            OverwritePolicy::Overwrite => {
                let same = matches!((std::fs::canonicalize(&path), std::fs::canonicalize(input)), (Ok(a), Ok(b)) if a == b);
                if same {
                    return receipt(Some(&path), "refused", Some("batch_output_would_replace_input"));
                }
            }
            OverwritePolicy::Ask => return receipt(Some(&path), "refused", Some("batch_ask_overwrite_in_headless")),
        }
    }
    if run.save_to(&path) {
        written.insert(path.clone());
        receipt(Some(&path), "written", None)
    } else {
        let code = last_error(run, "save_failed");
        receipt(Some(&path), "failed", Some(&code))
    }
}

/// Copies `input` into `folder` (never moves or deletes it); a taken name gets `-N`.
fn quarantine(input: &Path, folder: &Path) -> Result<String, &'static str> {
    std::fs::create_dir_all(folder).map_err(|_| "quarantine_unavailable")?;
    let name = input.file_name().ok_or("quarantine_no_file_name")?;
    let mut dest = folder.join(name);
    if dest.exists() {
        let stem = input.file_stem().and_then(|s| s.to_str()).unwrap_or("input");
        let ext = input.extension().and_then(|s| s.to_str()).map(|e| format!(".{e}")).unwrap_or_default();
        dest = (1..=MAX_RENAME_SUFFIX).map(|i| folder.join(format!("{stem}-{i}{ext}"))).find(|p| !p.exists()).ok_or("quarantine_rename_exhausted")?;
    }
    std::fs::copy(input, &dest).map_err(|_| "quarantine_copy_failed")?;
    Ok(dest.to_string_lossy().into_owned())
}
