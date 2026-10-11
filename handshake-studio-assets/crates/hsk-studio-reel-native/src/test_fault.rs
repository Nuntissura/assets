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
    /// Every decoded FRAME is answered 2 pixels wider than decoded (same height, zeroed planes): above
    /// the container-declared size but far under the reply cap, so only the declared-size check sees it.
    OversizePicture,
    /// Every decoded FRAME is answered 2 pixels narrower and 2 pixels lower than decoded (zeroed
    /// planes): a legal picture, because the declared size is the sequence maximum.
    UndersizePicture,
    /// OPEN: answer with a well-formed FRAME (2x2 mono, inside any declared size) instead of OPENED.
    FrameOnOpen,
    /// Every decoded FRAME is answered with a tag that no PACKET of the session carried (the top tag bit
    /// flipped; the picture itself is the decoded one, so only the tag check sees it).
    WrongTag,
    /// Before HELLO: a HELLO frame longer than the host's handshake cap (right kind and version, padded).
    OversizeHello,
    /// Before HELLO: bytes that are no frame at all (text); their first four bytes read as a length far
    /// above the handshake cap.
    GarbageHello,
    /// HELLO: a well-framed HELLO of the right kind whose body is cut short (a string longer than the
    /// bytes present), so the host's decoder rejects it.
    MalformedHello,
    /// Before HELLO: exit with code 86 (a worker that closes its output without ever greeting).
    ExitBeforeHello,
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
            Self::OversizePicture => Some("oversize_picture"),
            Self::UndersizePicture => Some("undersize_picture"),
            Self::FrameOnOpen => Some("frame_on_open"),
            Self::WrongTag => Some("wrong_tag"),
            Self::OversizeHello => Some("oversize_hello"),
            Self::GarbageHello => Some("garbage_hello"),
            Self::MalformedHello => Some("malformed_hello"),
            Self::ExitBeforeHello => Some("exit_before_hello"),
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
            Ok("oversize_picture") => Self::OversizePicture,
            Ok("undersize_picture") => Self::UndersizePicture,
            Ok("frame_on_open") => Self::FrameOnOpen,
            Ok("wrong_tag") => Self::WrongTag,
            Ok("oversize_hello") => Self::OversizeHello,
            Ok("garbage_hello") => Self::GarbageHello,
            Ok("malformed_hello") => Self::MalformedHello,
            Ok("exit_before_hello") => Self::ExitBeforeHello,
            _ => Self::None,
        }
    }
}
