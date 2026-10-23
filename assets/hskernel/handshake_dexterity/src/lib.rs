//! Pure profiles and launch plans. No filesystem discovery, authorization or process execution.
#![forbid(unsafe_code)]

mod plan;
mod profile;

pub use plan::{HostBinding, LaunchPlan, OutputContract, RuntimeCapability};
pub use profile::{Device, Engine, EngineProfile, Model, ModelFormat, Preset, Registry, ResolvedProfile, Sampling};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    InvalidIdentity,
    InvalidContentIdentity,
    InvalidBounds,
    IncompatibleProfile,
    DuplicateProfile,
    MissingProfile,
    InvalidWire,
    InvalidBinding,
    UnsupportedCapability,
    InvalidSelector,
    InvalidPrompt,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for Error {}

fn identity(value: &str) -> bool {
    !value.is_empty() && value.len() <= 256 && value.trim() == value
        && !value.chars().any(char::is_control)
}

fn content_identity(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        && value.bytes().any(|byte| byte != b'0')
}
