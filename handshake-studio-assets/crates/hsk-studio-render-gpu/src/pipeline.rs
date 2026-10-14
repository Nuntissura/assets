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
//! (tests/gpu_pipeline_fault.rs).
#![deny(clippy::arithmetic_side_effects)]
use crate::error::GpuError;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::Duration;

/// Upper bound of [`PipelineFault::message`] in bytes (backend compiler output can be long).
pub const FAULT_MESSAGE_MAX_BYTES: usize = 4096;

/// Plain-data detail of a failed shader-module or pipeline creation (no wgpu type, STU-ARC-002).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PipelineFault {
    /// The typed code, also carried by [`GpuError::Pipeline`]: `shader_validation` (the WGSL module
    /// was rejected), `pipeline_create` (a layout or the compute pipeline was rejected, including
    /// the backend shader compiler), `pipeline_panic` (wgpu panicked inside the call) or
    /// `pipeline_scope_timeout` (an error scope did not resolve).
    pub code: &'static str,
    /// The wgpu label of the object whose creation failed.
    pub label: &'static str,
    /// wgpu error class: `validation`, `internal`, `out_of_memory`, `panic`, `timeout` or
    /// `uncaptured` (an error that bypassed the scopes and reached the device's handler).
    pub class: &'static str,
    /// The backend message (wgpu's error description, with the backend compiler output), cut to at
    /// most [`FAULT_MESSAGE_MAX_BYTES`] on a character boundary.
    pub message: String,
}

impl PipelineFault {
    /// The `Copy` error of this fault: `GpuError::Pipeline(code)`.
    pub const fn error(&self) -> GpuError {
        GpuError::Pipeline(self.code)
    }
}

impl std::fmt::Display for PipelineFault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}({}, {}): {}", self.code, self.label, self.class, self.message)
    }
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

fn panic_text(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(text) = payload.downcast_ref::<&str>() {
        (*text).to_owned()
    } else if let Some(text) = payload.downcast_ref::<String>() {
        text.clone()
    } else {
        String::from("non-text panic payload")
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
    let fault = |class: &'static str, message: &str| PipelineFault {
        code: stage.code(),
        label,
        class,
        message: bounded_message(message),
    };
    device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
    device.push_error_scope(wgpu::ErrorFilter::Internal);
    device.push_error_scope(wgpu::ErrorFilter::Validation);
    let made = catch_unwind(AssertUnwindSafe(create));
    // Pop all three (innermost first) whatever happened, so no scope leaks into later calls.
    let mut first: Option<PipelineFault> = None;
    for _ in 0..3 {
        let popped = crate::device::block_on_bounded(device.pop_error_scope(), Some(SCOPE_TIMEOUT));
        let found = match popped {
            Some(None) => None,
            Some(Some(error)) => Some(fault(class_of(&error), &error.to_string())),
            None => Some(PipelineFault {
                code: "pipeline_scope_timeout",
                ..fault("timeout", "error scope did not resolve")
            }),
        };
        if first.is_none() {
            first = found;
        }
    }
    match (made, first) {
        (Err(payload), _) => Err(PipelineFault {
            code: "pipeline_panic",
            ..fault("panic", &panic_text(payload.as_ref()))
        }),
        (Ok(_), Some(failure)) => Err(failure),
        (Ok(value), None) => Ok(value),
    }
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
