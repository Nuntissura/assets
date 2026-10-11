//! Worker fault injection for this crate's supervision tests. Compiled only with the non-default
//! `test-faults` feature (V4-04): product builds have neither this type, nor the
//! `Av1WorkerOptions::test_fault` field, nor the worker's read of the environment variable, so a
//! product worker ignores `HSK_REEL_NATIVE_AV1_WORKER_TEST_FAULT` (`tests/worker_fault_env.rs`).

/// Fault the worker injects into its first request (set by the host through `ENV` for the child it
/// spawns; the child environment is otherwise empty).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WorkerTestFault {
    #[default]
    None,
    /// First PACKET: sleep (no reply) so the host deadline or cancel has to kill the worker.
    HangOnPacket,
    /// First PACKET: exit with code 86 (a worker that dies mid-request).
    ExitOnPacket,
    /// OPEN: exit with code 86.
    ExitOnOpen,
    /// First PACKET: answer with a FRAME far above any declared-dimension cap.
    OversizeFrame,
    /// First PACKET: answer with a frame of unknown kind.
    GarbageReply,
    /// Before HELLO: sleep (no HELLO) so the host's spawn wait has to be cancelled or time out.
    StallHello,
}

impl WorkerTestFault {
    pub const ENV: &'static str = "HSK_REEL_NATIVE_AV1_WORKER_TEST_FAULT";

    pub(crate) const fn env_value(self) -> Option<&'static str> {
        match self {
            Self::None => None,
            Self::HangOnPacket => Some("hang_on_packet"),
            Self::ExitOnPacket => Some("exit_on_packet"),
            Self::ExitOnOpen => Some("exit_on_open"),
            Self::OversizeFrame => Some("oversize_frame"),
            Self::GarbageReply => Some("garbage_reply"),
            Self::StallHello => Some("stall_hello"),
        }
    }

    #[cfg(feature = "av1-worker")]
    pub(crate) fn from_env() -> Self {
        match std::env::var(Self::ENV).as_deref() {
            Ok("hang_on_packet") => Self::HangOnPacket,
            Ok("exit_on_packet") => Self::ExitOnPacket,
            Ok("exit_on_open") => Self::ExitOnOpen,
            Ok("oversize_frame") => Self::OversizeFrame,
            Ok("garbage_reply") => Self::GarbageReply,
            Ok("stall_hello") => Self::StallHello,
            _ => Self::None,
        }
    }
}
