use crate::error::PluginError;
use hsk_studio_accord::{ActorContext, CancellationToken};
use hsk_studio_score::{
    ChannelLayout, InputBlock, MAX_BLOCK_FRAMES, MAX_SAMPLE_RATE_HZ, OutputBlock,
};
use std::{path::PathBuf, sync::Arc, time::Duration};

/// ABI version of the CLAP plugin ABI this provider speaks (CLAP major version).
pub const SUPPORTED_CLAP_ABI: u32 = 1;
/// Largest per-call deadline a caller may request.
pub const MAX_DEADLINE: Duration = Duration::from_secs(60);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PluginFormat {
    Clap,
    /// Declared for the approval record; no worker backend supports it yet (`abi_mismatch`).
    Vst3,
}

impl PluginFormat {
    pub(crate) const fn wire(self) -> u8 {
        match self {
            Self::Clap => 1,
            Self::Vst3 => 2,
        }
    }
    pub(crate) const fn from_wire(value: u8) -> Option<Self> {
        match value {
            1 => Some(Self::Clap),
            2 => Some(Self::Vst3),
            _ => None,
        }
    }
}

/// CPU architecture the approved binary was built for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arch {
    X86_64,
    Aarch64,
}

impl Arch {
    /// Architecture of this build, `None` on a target the provider does not know.
    pub const fn host() -> Option<Self> {
        if cfg!(target_arch = "x86_64") {
            Some(Self::X86_64)
        } else if cfg!(target_arch = "aarch64") {
            Some(Self::Aarch64)
        } else {
            None
        }
    }
    pub(crate) const fn wire(self) -> u8 {
        match self {
            Self::X86_64 => 1,
            Self::Aarch64 => 2,
        }
    }
}

/// A plugin file the caller (the Handshake-wide plugin platform) has approved by content hash.
/// There is no discovery in this crate: no scan, no registry, no path without a hash.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApprovedPlugin {
    pub path: PathBuf,
    pub content_sha256: [u8; 32],
    pub format: PluginFormat,
    pub arch: Arch,
    pub abi_version: u32,
}

/// The caller's authorization for ONE content hash, attributed to an actor. The attribution strings
/// are transport identifiers, not proof of a host grant (accord); the caller enforces the grant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginGrant {
    pub actor: ActorContext,
    pub content_sha256: [u8; 32],
}

/// What a probe learned about a plugin, from inside an isolated worker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginDescriptor {
    pub plugin_id: String,
    pub name: String,
    pub format: PluginFormat,
    pub abi_version: u32,
    pub inputs: u16,
    pub outputs: u16,
    /// Identity of the worker backend that produced this descriptor.
    pub provider_identity: String,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Parameter {
    pub id: u32,
    pub value: f64,
}

/// Everything an activation needs; validated before any process is spawned.
#[derive(Clone, Debug)]
pub struct ProcessConfig {
    pub plugin: ApprovedPlugin,
    pub grant: Option<PluginGrant>,
    pub sample_rate_hz: u32,
    pub layout: ChannelLayout,
    /// Preallocated block bound: the instance refuses blocks above it.
    pub max_block_frames: usize,
    /// Initial parameter values (state version 1 includes them).
    pub parameters: Vec<Parameter>,
    /// Bound on one captured state blob; also part of the admitted in-flight bytes.
    pub max_state_bytes: u32,
}

impl ProcessConfig {
    pub(crate) fn validate(&self) -> Result<(), PluginError> {
        if self.sample_rate_hz == 0
            || self.sample_rate_hz > MAX_SAMPLE_RATE_HZ
            || self.max_block_frames == 0
            || self.max_block_frames > MAX_BLOCK_FRAMES
            || self.parameters.len() > 256
            || self.parameters.iter().any(|p| !p.value.is_finite())
        {
            return Err(PluginError::InvalidConfig);
        }
        Ok(())
    }
}

/// Completeness of the plugin state a block or capture reflects (STU-ARC-021).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Completeness {
    Complete,
    Partial,
    Unavailable,
}

impl Completeness {
    pub(crate) const fn wire(self) -> u8 {
        match self {
            Self::Complete => 1,
            Self::Partial => 2,
            Self::Unavailable => 3,
        }
    }
    pub(crate) const fn from_wire(value: u8) -> Option<Self> {
        match value {
            1 => Some(Self::Complete),
            2 => Some(Self::Partial),
            3 => Some(Self::Unavailable),
            _ => None,
        }
    }
}

/// An immutable, versioned snapshot of plugin state captured off the audio callback.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StateCapture {
    pub state_version: u64,
    pub completeness: Completeness,
    pub bytes: Vec<u8>,
}

/// Receipt of one block processed by an isolated plugin.
#[derive(Clone, Debug, PartialEq)]
pub struct ProcessReceipt {
    pub instance_id: u64,
    pub frames: usize,
    pub start_sample: u64,
    /// Delay of the plugin output relative to its input, in samples.
    pub latency_samples: u32,
    /// State version the block was processed under; bound to `instance_id`.
    pub state_version: u64,
    pub provider_identity: Arc<str>,
    pub completeness: Completeness,
}

/// Why a call failed and what happened to the worker; the output block held silence, never
/// partial plugin data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FaultReport {
    pub error: PluginError,
    pub killed: bool,
    pub reaped: bool,
    pub exit_code: Option<i32>,
    /// True when the caller's output block was overwritten with silence.
    pub safe_output: bool,
}

/// Final accounting of one instance.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetireReport {
    pub instance_id: u64,
    pub pid: Option<u32>,
    pub blocks_processed: u64,
    pub killed: bool,
    pub reaped: bool,
    pub exit_code: Option<i32>,
    /// Admission permits (worker slot and in-flight bytes) given back, only after the reap.
    pub released_bytes: u64,
}

/// Lifecycle of one hosted plugin. Transitions are checked: a call in the wrong state is an
/// `IllegalTransition`, never a silent no-op.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LifecycleState {
    Discovered,
    Probed,
    Active,
    Processing,
    Draining,
    Retired,
}

impl LifecycleState {
    pub const fn allows(self, to: Self) -> bool {
        matches!(
            (self, to),
            (Self::Discovered, Self::Probed | Self::Draining)
                | (Self::Probed, Self::Active | Self::Draining)
                | (Self::Active, Self::Processing | Self::Draining)
                | (Self::Processing, Self::Active | Self::Draining)
                | (Self::Draining, Self::Retired)
        )
    }

    pub fn transition(self, to: Self) -> Result<Self, PluginError> {
        if self.allows(to) {
            Ok(to)
        } else {
            Err(PluginError::IllegalTransition { from: self, to })
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::Discovered => "discovered",
            Self::Probed => "probed",
            Self::Active => "active",
            Self::Processing => "processing",
            Self::Draining => "draining",
            Self::Retired => "retired",
        }
    }
}

/// Port to a plugin host. The only implementation here is [`IsolatedHost`](crate::IsolatedHost):
/// there is no in-process implementation and no fallback to one (PLATFORM-08).
pub trait PluginProvider: Send + Sync {
    /// Runs the plugin's probe inside an isolated worker; the host process never loads the file.
    fn probe(
        &self,
        plugin: &ApprovedPlugin,
        grant: Option<&PluginGrant>,
        cancel: &CancellationToken,
        deadline: Duration,
    ) -> Result<PluginDescriptor, PluginError>;

    /// Starts a worker for `config`, activates the plugin at the negotiated rate and block bound.
    fn activate(
        &self,
        config: &ProcessConfig,
        cancel: &CancellationToken,
        deadline: Duration,
    ) -> Result<Box<dyn PluginInstance>, PluginError>;
}

/// One activated plugin. Every method is OFF the audio callback (STU-ARC-013): it may allocate,
/// wait and kill a worker.
pub trait PluginInstance: Send {
    fn lifecycle(&self) -> LifecycleState;
    fn instance_id(&self) -> u64;
    fn provider_identity(&self) -> &str;
    fn latency_samples(&self) -> u32;
    fn state_version(&self) -> u64;
    fn layout(&self) -> &ChannelLayout;
    fn sample_rate_hz(&self) -> u32;
    fn max_block_frames(&self) -> usize;

    /// Processes one block prepared under `expected_state_version`. On success the plugin's
    /// output is in the first `input.frames()` samples of every `out` channel (the caller or a
    /// score `Engine` publishes it). On any failure those samples hold silence.
    fn process_expecting(
        &mut self,
        expected_state_version: u64,
        input: &InputBlock<'_>,
        out: &mut OutputBlock,
        cancel: &CancellationToken,
        deadline: Duration,
    ) -> Result<ProcessReceipt, PluginError>;

    /// Processes one block under the instance's current state version.
    fn process(
        &mut self,
        input: &InputBlock<'_>,
        out: &mut OutputBlock,
        cancel: &CancellationToken,
        deadline: Duration,
    ) -> Result<ProcessReceipt, PluginError> {
        let version = self.state_version();
        self.process_expecting(version, input, out, cancel, deadline)
    }

    /// Changes one parameter; returns the new state version.
    fn set_parameter(
        &mut self,
        parameter: Parameter,
        cancel: &CancellationToken,
        deadline: Duration,
    ) -> Result<u64, PluginError>;

    /// Captures a versioned state snapshot of at most `max_bytes`.
    fn capture_state(
        &mut self,
        cancel: &CancellationToken,
        deadline: Duration,
        max_bytes: u32,
    ) -> Result<StateCapture, PluginError>;

    /// The fault that ended or interrupted the last failed call, if any.
    fn last_fault(&self) -> Option<&FaultReport>;

    /// Ends the worker, reclaims it (kill after the grace period) and gives the permits back.
    fn retire(self: Box<Self>) -> RetireReport;
}
