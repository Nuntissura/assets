//! The renderer-free UI toolkit (decisions O4, O5, `direction.blocker_decisions_20261107`): widgets, display list,
//! focus, gestures and IME composition state. Toolkit-free crates are named `kumihimo-*` from the first commit and take
//! no `hsk-studio-*` dependency; Studio crates depend on them, not the reverse (`tools/workspace-probe.rs --self-test`).
//!
//! Skeleton: no API yet. The package keeps literal version fields so it can leave this workspace when it is published.
#![forbid(unsafe_code)]

/// Self description for models and tooling.
pub const DESCRIPTOR: &str = r#"{"schema":"kumihimo.ui.descriptor@1","crate":"kumihimo-ui","slice":0,"state":"skeleton, no API yet","owns":"widgets, display list, focus, gestures, IME composition state","closure":"no hsk-studio-* crate, checked by the workspace quarantine gate","decisions":"O4 O5"}"#;
