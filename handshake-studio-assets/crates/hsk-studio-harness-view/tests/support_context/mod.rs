//! Test host attribution (lane SMB-ATTRIBUTION-1): these tests are their own host, so they build
//! their viewers with `Viewer::with_context` and their own `RunContext` (a test actor, client
//! session `harness-view-tests`, fresh in-memory correlations) instead of the feature-gated
//! `Viewer::new`, and run in every round unit without the `asset-local-test-host` feature
//! (rule 4e).
#![allow(dead_code)]
use hsk_studio_accord::ActorContext;
use hsk_studio_harness_core::{ClientSessionId, RunContext, TransientCorrelation};
use hsk_studio_harness_view::view::Viewer;
use hsk_studio_harness_view::view::render::Prefer;
use std::sync::Arc;

pub fn context() -> RunContext {
    let actor = ActorContext::new("account", "principal", "owner", "owner-principal", "space", "session").unwrap();
    RunContext::new(actor, ClientSessionId::parse("harness-view-tests").unwrap(), Arc::new(TransientCorrelation::new()))
}

/// `Viewer::new(prefer)` on this test host's context.
pub fn viewer(prefer: Prefer) -> Viewer {
    Viewer::with_context(prefer, hsk_studio_render_gpu::DeviceOptions::default(), context())
}

/// `Viewer::with_gpu_options(prefer, options)` on this test host's context.
pub fn viewer_with(prefer: Prefer, options: hsk_studio_render_gpu::DeviceOptions) -> Viewer {
    Viewer::with_context(prefer, options, context())
}
