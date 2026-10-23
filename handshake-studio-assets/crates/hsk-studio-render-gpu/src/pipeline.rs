//! Typed shader-module and pipeline creation (lane SMB-RENDER-GPU-PIPELINE-FAULT).
//!
//! wgpu 27 (wgpu-core 27.0.3) routes a creation error to the innermost error scope whose filter
//! matches the error's class, else to the device's uncaptured-error handler, else to its default
//! handler, which panics (`wgpu::backend::wgpu_core::ErrorSinkRaw::handle_error_or_return_handler`).
//! A WGSL module that naga rejects is a `Validation` error, but a backend compiler failure (DX12 FXC
//! or DXC `D3DCompile`, a Vulkan/Metal driver rejection) is `hal::PipelineError::Linkage`, which
//! wgpu-core maps to `CreateComputePipelineError::Internal`, class `Internal`. A `Validation`-only
//! scope (the pattern of wgpu's own test helpers `fail`/`valid`) therefore does not catch it: on a
//! device with our uncaptured handler it latched a fault and the invalid pipeline surfaced as
//! `device_fault` at the first dispatch; on a host-owned device without a handler it panicked.
//!
//! [`guarded`] wraps ONE creation call in three scopes (out of memory, internal, validation), pops
//! all three, and catches a panic of the call, so every failure class becomes a [`PipelineFault`]
//! with the object's label and the backend's message, before any dispatch. Nothing reaches the
//! uncaptured handler, so the device is not marked faulted and stays usable for other pipelines
//! (tests/gpu_pipeline_fault.rs). The same scopes and a panic guard cover execution: every chunk
//! of `plan_sink::run` runs under [`ErrorScopes`], so an error raised while its commands are
//! encoded, submitted or read back is a typed `submit_fault` with its detail (lane
//! SMB-RENDER-GPU-SUBMIT-FAULT, tests/gpu_submit_fault.rs).
#![deny(clippy::arithmetic_side_effects)]
use crate::error::GpuError;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::Duration;


/// Upper bound of [`PipelineFault::message`] in bytes (backend compiler output can be long).
pub const FAULT_MESSAGE_MAX_BYTES: usize = 4096;

/// Plain-data detail of a typed device failure (no wgpu type, STU-ARC-002): a failed shader-module
/// or pipeline creation (lane SMB-RENDER-GPU-PIPELINE-FAULT) or an error raised while a chunk's
/// commands were encoded, submitted or read back (lane SMB-RENDER-GPU-SUBMIT-FAULT).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PipelineFault {
    /// The typed code, also carried by [`PipelineFault::error`]. Creation (`GpuError::Pipeline`):
    /// `shader_validation` (the WGSL module was rejected), `pipeline_create` (a layout or the
    /// compute pipeline was rejected, including the backend shader compiler), `pipeline_panic`
    /// (wgpu panicked inside the call), `pipeline_scope_timeout` (an error scope did not resolve).
    /// Execution (`GpuError::Gpu`): `submit_fault` (an error captured while a chunk's commands were
    /// encoded or submitted), `readback_map_failed` (the read-back mapping failed), `submit_panic`
    /// (a panic inside the execution, caught; the device is then treated as lost),
    /// `submit_scope_timeout`.
    pub code: &'static str,
    /// The operation that failed: the wgpu label of the created object, or the execution step
    /// (`hsk-studio-render-gpu chunk` = the chunk's passes, `hsk-studio-render-gpu readback`,
    /// `hsk-studio-render-gpu shared output`, `hsk-studio-render-gpu execute` = a caught panic).
    pub label: &'static str,
    /// The label wgpu reports for the failing object (see `reported_label`), else the operation
    /// label.
    pub object: String,
    /// wgpu error class: `validation`, `internal`, `out_of_memory`, `panic`, `timeout`, `map` (a
    /// failed mapping without a scoped error) or `uncaptured` (an error that bypassed the scopes).
    pub class: &'static str,
    /// The backend message (wgpu's error description, with the backend compiler output), machine
    /// paths reduced to their file name ([`redact_paths`]), then cut to at most
    /// [`FAULT_MESSAGE_MAX_BYTES`] on a character boundary.
    pub message: String,
}

impl PipelineFault {
    /// Build a fault: the message is redacted ([`redact_paths`]) and bounded, and `object` is the
    /// object label wgpu names in the redacted message (else `label`).
    pub fn new(code: &'static str, label: &'static str, class: &'static str, message: &str) -> Self {
        let message = bounded_message(&redact_paths(message));
        let object = reported_label(&message).unwrap_or(label).to_owned();
        Self { code, label, object, class, message }
    }

    /// The `Copy` error of this fault: `GpuError::Pipeline(code)` for a creation code,
    /// `GpuError::Gpu(code)` for an execution code.
    pub fn error(&self) -> GpuError {
        match self.code {
            "shader_validation" | "pipeline_create" | "pipeline_panic" | "pipeline_scope_timeout" => {
                GpuError::Pipeline(self.code)
            }
            other => GpuError::Gpu(other),
        }
    }
}

impl std::fmt::Display for PipelineFault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}({}, {}): {}", self.code, self.label, self.class, self.message)
    }
}

/// The first `label = '...'` of a wgpu message (wgpu's `ContextError` prints the failing object's
/// label that way), else the first `with '...'` (`Buffer with '...' label has been destroyed`).
fn reported_label(message: &str) -> Option<&str> {
    quoted_after(message, "label = '").or_else(|| quoted_after(message, " with '"))
}

/// The non-empty text between `key` (ending in a quote) and the next `'`.
fn quoted_after<'m>(message: &'m str, key: &str) -> Option<&'m str> {
    let start = message.find(key)?.checked_add(key.len())?;
    let rest = message.get(start..)?;
    let end = rest.find('\'')?;
    rest.get(..end).filter(|label| !label.is_empty())
}

/// The kind of path that starts at a position of [`redact_paths`]' input.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PathKind {
    /// `X:\` or `X:/` (spaces allowed inside, as Windows paths have them).
    Drive,
    /// `\\server\share` (spaces allowed inside).
    Unc,
    /// `/abs/path` (ends at whitespace).
    Unix,
}

fn path_start(chars: &[char], i: usize) -> Option<PathKind> {
    let at = |k: usize| i.checked_add(k).and_then(|j| chars.get(j)).copied();
    let before = i.checked_sub(1).and_then(|j| chars.get(j)).copied();
    let boundary = before.is_none_or(|c| !c.is_alphanumeric() && c != '\\' && c != '/');
    let c0 = at(0)?;
    if boundary
        && c0.is_ascii_alphabetic()
        && at(1) == Some(':')
        && matches!(at(2), Some('\\' | '/'))
    {
        return Some(PathKind::Drive);
    }
    if boundary && c0 == '\\' && at(1) == Some('\\') {
        return Some(PathKind::Unc);
    }
    let unix_boundary =
        before.is_none_or(|c| c.is_whitespace() || matches!(c, '\'' | '"' | '(' | '[' | '=' | ',' | '`'));
    if c0 == '/' && unix_boundary && at(1).is_some_and(|c| !c.is_whitespace() && c != '/') {
        return Some(PathKind::Unix);
    }
    None
}

/// Reduce every machine path in `text` to its file name: `D:\a b\c\shader x(4,5): error` becomes
/// `shader x(4,5): error`, `/home/u/k.wgsl:3` becomes `k.wgsl:3`, `\\host\share\f` becomes `f`.
/// A path starts at a drive letter (`X:\`, `X:/`), a UNC prefix (`\\`) or a `/` after whitespace,
/// a quote, `(`, `[`, `=`, `,` or the start, and runs to the end of the line or the first character
/// that cannot be inside a path (`: " ' < > | ? *`, and whitespace for a `/` path); everything up to
/// its last separator is removed. Text without a path is returned unchanged; `and/or`, `a / b` and
/// URLs (`https://...`) are not paths (test redact_paths_keeps_only_file_names). The single place
/// where backend messages lose machine paths before they enter fault data.
pub fn redact_paths(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0_usize;
    while let Some(&c) = chars.get(i) {
        if let Some(kind) = path_start(&chars, i) {
            let skip = if kind == PathKind::Drive { 2 } else { 0 };
            let mut end = i.saturating_add(skip);
            let mut last_separator = None;
            while let Some(&p) = chars.get(end) {
                let stops = matches!(p, '\n' | '\r' | ':' | '"' | '\'' | '<' | '>' | '|' | '?' | '*')
                    || (kind == PathKind::Unix && p.is_whitespace());
                if stops {
                    break;
                }
                if p == '\\' || p == '/' {
                    last_separator = Some(end);
                }
                end = end.saturating_add(1);
            }
            if let Some(separator) = last_separator {
                i = separator.saturating_add(1);
                continue;
            }
        }
        out.push(c);
        i = i.saturating_add(1);
    }
    out
}

/// What a guarded call creates (selects the code of a failure).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Stage {
    /// `create_shader_module`: a failure is `shader_validation`.
    Module,
    /// Layouts and `create_compute_pipeline`: a failure is `pipeline_create`.
    Pipeline,
}

impl Stage {
    const fn code(self) -> &'static str {
        match self {
            Self::Module => "shader_validation",
            Self::Pipeline => "pipeline_create",
        }
    }
}

/// Bound on popping one error scope (the native futures are ready on the first poll).
const SCOPE_TIMEOUT: Duration = Duration::from_secs(5);

pub(crate) fn bounded_message(text: &str) -> String {
    if text.len() <= FAULT_MESSAGE_MAX_BYTES {
        return text.to_owned();
    }
    let mut end = FAULT_MESSAGE_MAX_BYTES;
    while !text.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    text.get(..end).unwrap_or_default().to_owned()
}

fn class_of(error: &wgpu::Error) -> &'static str {
    match error {
        wgpu::Error::Validation { .. } => "validation",
        wgpu::Error::Internal { .. } => "internal",
        wgpu::Error::OutOfMemory { .. } => "out_of_memory",
    }
}

pub(crate) fn panic_text(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(text) = payload.downcast_ref::<&str>() {
        (*text).to_owned()
    } else if let Some(text) = payload.downcast_ref::<String>() {
        text.clone()
    } else {
        String::from("non-text panic payload")
    }
}

/// What a set of error scopes captured: `(class, message)`.
pub(crate) type Captured = (&'static str, String);

/// Out-of-memory, internal and validation error scopes on `device`, popped by
/// [`ErrorScopes::finish`] or, on an early return or an unwind, by `Drop`, so no scope leaks into
/// later calls. wgpu 27 scopes are per device (not per thread): a host that shares the device and
/// raises errors on another thread meanwhile can have them captured here.
pub(crate) struct ErrorScopes<'a> {
    device: &'a wgpu::Device,
    open: bool,
}

impl<'a> ErrorScopes<'a> {
    pub(crate) fn push(device: &'a wgpu::Device) -> Self {
        device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
        device.push_error_scope(wgpu::ErrorFilter::Internal);
        device.push_error_scope(wgpu::ErrorFilter::Validation);
        Self { device, open: true }
    }

    /// Pop all three scopes (innermost first); the first captured error, a timeout, or `None`.
    pub(crate) fn finish(mut self) -> Option<Captured> {
        self.open = false;
        let mut first = None;
        for _ in 0..3 {
            let popped =
                crate::device::block_on_bounded(self.device.pop_error_scope(), Some(SCOPE_TIMEOUT));
            let found = match popped {
                Some(None) => None,
                Some(Some(error)) => Some((class_of(&error), error.to_string())),
                None => Some(("timeout", String::from("error scope did not resolve"))),
            };
            if first.is_none() {
                first = found;
            }
        }
        first
    }
}

impl Drop for ErrorScopes<'_> {
    fn drop(&mut self) {
        if self.open {
            self.open = false;
            for _ in 0..3 {
                let _ = crate::device::block_on_bounded(
                    self.device.pop_error_scope(),
                    Some(SCOPE_TIMEOUT),
                );
            }
        }
    }
}

/// Run one creation call `create` under out-of-memory, internal and validation error scopes and a
/// panic guard. `Ok` only when the call returned and no scope captured an error.
pub(crate) fn guarded<T>(
    device: &wgpu::Device,
    label: &'static str,
    stage: Stage,
    create: impl FnOnce() -> T,
) -> Result<T, PipelineFault> {
    let scopes = ErrorScopes::push(device);
    let made = catch_unwind(AssertUnwindSafe(create));
    let captured = scopes.finish();
    match (made, captured) {
        (Err(payload), _) => Err(PipelineFault::new(
            "pipeline_panic",
            label,
            "panic",
            &panic_text(payload.as_ref()),
        )),
        (Ok(_), Some(("timeout", message))) => Err(PipelineFault::new(
            "pipeline_scope_timeout",
            label,
            "timeout",
            &message,
        )),
        (Ok(_), Some((class, message))) => {
            Err(PipelineFault::new(stage.code(), label, class, &message))
        }
        (Ok(value), None) => Ok(value),
    }
}

/// Faults a test can inject into one execution (feature `test-faults`, V4-04), each a real wgpu
/// failure on the live device, consumed by the next execution.
#[cfg(feature = "test-faults")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InjectedFault {
    /// The first chunk's read-back copy is 16 bytes longer than the staging buffer: wgpu raises a
    /// validation error when the command encoder is finished / submitted.
    ReadbackCopyOutOfBounds,
    /// The staging buffer is destroyed before the first chunk is mapped: the mapping fails.
    StagingDestroyed,
    /// A panic inside the first chunk (stands in for a wgpu-internal panic such as the fatal
    /// `Buffer::get_mapped_range` / `Device::poll` paths).
    Panic,
}

/// Build one compute pipeline from `wgsl` with an automatic layout on `device`, guarded. Test hook
/// of `GpuBackend::probe_pipeline` (feature `test-faults`).
#[cfg(feature = "test-faults")]
pub(crate) fn probe(device: &wgpu::Device, wgsl: &str, entry_point: &str) -> Result<(), PipelineFault> {
    const LABEL: &str = "hsk-studio-render-gpu probe";
    let module = guarded(device, LABEL, Stage::Module, || {
        device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some(LABEL),
            source: wgpu::ShaderSource::Wgsl(std::borrow::Cow::Owned(wgsl.to_owned())),
        })
    })?;
    guarded(device, LABEL, Stage::Pipeline, || {
        device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some(LABEL),
            layout: None,
            module: &module,
            entry_point: Some(entry_point),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        })
    })?;
    Ok(())
}
