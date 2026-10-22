//! Fail-closed quota configuration: zero and unlimited values are rejected (STU-MOT-258).
use std::time::Duration;

pub const MAX_SOURCE_BYTES_CEILING: usize = 1 << 20;
pub const MEMORY_BYTES_CEILING: u64 = 1 << 30;
pub const STACK_BYTES_CEILING: u64 = 64 << 20;
pub const RECURSION_CEILING: u32 = 100_000;
pub const OUTPUT_NODES_CEILING: usize = 1 << 20;
pub const OUTPUT_DEPTH_CEILING: usize = 64;
pub const HOST_READS_CEILING: u32 = 65_536;
pub const DEADLINE_CEILING: Duration = Duration::from_secs(10);
pub const REALMS_CEILING: u32 = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigError {
    ZeroLimit(&'static str),
    AboveCeiling(&'static str),
}

impl ConfigError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::ZeroLimit(_) => "zero_limit",
            Self::AboveCeiling(_) => "above_ceiling",
        }
    }
}

/// Unvalidated request; only `Quotas::try_from_spec` turns it into a usable limit set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QuotaSpec {
    pub max_source_bytes: usize,
    pub memory_bytes: u64,
    pub stack_bytes: u64,
    pub recursion_limit: u32,
    pub max_output_nodes: usize,
    pub max_output_depth: usize,
    pub max_host_reads: u32,
    pub deadline: Duration,
    pub max_concurrent_realms: u32,
}

impl Default for QuotaSpec {
    fn default() -> Self {
        Self {
            max_source_bytes: 65_536,
            memory_bytes: 16 << 20,
            stack_bytes: 512 << 10,
            recursion_limit: 256,
            max_output_nodes: 4096,
            max_output_depth: 8,
            max_host_reads: 1024,
            deadline: Duration::from_millis(250),
            max_concurrent_realms: 1,
        }
    }
}

/// Validated, nonzero, bounded limits. Fields are private so no zero/unlimited set can exist.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Quotas {
    max_source_bytes: usize,
    memory_bytes: u64,
    stack_bytes: u64,
    recursion_limit: u32,
    max_output_nodes: usize,
    max_output_depth: usize,
    max_host_reads: u32,
    deadline: Duration,
    max_concurrent_realms: u32,
}

fn check<T: PartialOrd + Default>(name: &'static str, value: T, ceiling: T) -> Result<T, ConfigError> {
    if value == T::default() {
        Err(ConfigError::ZeroLimit(name))
    } else if value > ceiling {
        Err(ConfigError::AboveCeiling(name))
    } else {
        Ok(value)
    }
}

impl Quotas {
    pub fn try_from_spec(spec: QuotaSpec) -> Result<Self, ConfigError> {
        Ok(Self {
            max_source_bytes: check("max_source_bytes", spec.max_source_bytes, MAX_SOURCE_BYTES_CEILING)?,
            memory_bytes: check("memory_bytes", spec.memory_bytes, MEMORY_BYTES_CEILING)?,
            stack_bytes: check("stack_bytes", spec.stack_bytes, STACK_BYTES_CEILING)?,
            recursion_limit: check("recursion_limit", spec.recursion_limit, RECURSION_CEILING)?,
            max_output_nodes: check("max_output_nodes", spec.max_output_nodes, OUTPUT_NODES_CEILING)?,
            max_output_depth: check("max_output_depth", spec.max_output_depth, OUTPUT_DEPTH_CEILING)?,
            max_host_reads: check("max_host_reads", spec.max_host_reads, HOST_READS_CEILING)?,
            deadline: check("deadline", spec.deadline, DEADLINE_CEILING)?,
            max_concurrent_realms: check("max_concurrent_realms", spec.max_concurrent_realms, REALMS_CEILING)?,
        })
    }

    pub const fn max_source_bytes(&self) -> usize {
        self.max_source_bytes
    }

    pub const fn memory_bytes(&self) -> u64 {
        self.memory_bytes
    }

    pub const fn stack_bytes(&self) -> u64 {
        self.stack_bytes
    }

    pub const fn recursion_limit(&self) -> u32 {
        self.recursion_limit
    }

    pub const fn max_output_nodes(&self) -> usize {
        self.max_output_nodes
    }

    pub const fn max_output_depth(&self) -> usize {
        self.max_output_depth
    }

    pub const fn max_host_reads(&self) -> u32 {
        self.max_host_reads
    }

    pub const fn deadline(&self) -> Duration {
        self.deadline
    }

    pub const fn max_concurrent_realms(&self) -> u32 {
        self.max_concurrent_realms
    }
}

impl Default for Quotas {
    fn default() -> Self {
        let spec = QuotaSpec::default();
        Self {
            max_source_bytes: spec.max_source_bytes,
            memory_bytes: spec.memory_bytes,
            stack_bytes: spec.stack_bytes,
            recursion_limit: spec.recursion_limit,
            max_output_nodes: spec.max_output_nodes,
            max_output_depth: spec.max_output_depth,
            max_host_reads: spec.max_host_reads,
            deadline: spec.deadline,
            max_concurrent_realms: spec.max_concurrent_realms,
        }
    }
}
