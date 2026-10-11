use hsk_studio_observe::{FailureCode, Outcome};

/// Stable, content-free failure vocabulary for the device provider. `code()` strings are
/// wire-stable snake_case. Every refusal happens before any ring, thread or permit is created.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceError {
    /// No grant was supplied, or the grant does not cover this direction or device.
    Unauthorized,
    /// Device id is empty, too long or carries control characters.
    InvalidDeviceId,
    /// Sample rate is zero, above the Score bound or not an exact tick divisor.
    InvalidRate,
    /// Block frame count is zero or above the configured bound.
    InvalidBlockFrames,
    /// Ring block count is below two or above the configured bound.
    InvalidRingBlocks,
    /// The fixed ring would exceed `Limits::max_ring_bytes` (or overflow the size arithmetic).
    RingTooLarge,
    /// The device cannot carry this channel layout; no silent downmix or truncation.
    UnsupportedLayout,
    /// The provider does not know this device.
    UnknownDevice,
    /// The provider or device cannot serve the request (no device, unsupported sample format).
    Unavailable,
    /// Bounded admission (open streams) is exhausted.
    AdmissionDenied,
    /// The device was lost after it was opened.
    DeviceLost,
    /// Cancellation was observed before any resource was created.
    Canceled,
}

impl DeviceError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Unauthorized => "unauthorized",
            Self::InvalidDeviceId => "invalid_device_id",
            Self::InvalidRate => "invalid_rate",
            Self::InvalidBlockFrames => "invalid_block_frames",
            Self::InvalidRingBlocks => "invalid_ring_blocks",
            Self::RingTooLarge => "ring_too_large",
            Self::UnsupportedLayout => "unsupported_layout",
            Self::UnknownDevice => "unknown_device",
            Self::Unavailable => "unavailable",
            Self::AdmissionDenied => "admission_denied",
            Self::DeviceLost => "device_lost",
            Self::Canceled => "canceled",
        }
    }

    /// Diagnostic class: caller mistakes are Validation, a layout the device cannot carry is
    /// Unsupported, absent or exhausted providers are Unavailable, a lost device is Loss.
    pub const fn outcome(self) -> Outcome {
        match self {
            Self::Canceled => Outcome::Canceled,
            Self::Unauthorized
            | Self::InvalidDeviceId
            | Self::InvalidRate
            | Self::InvalidBlockFrames
            | Self::InvalidRingBlocks
            | Self::RingTooLarge => Outcome::Failure(FailureCode::Validation),
            Self::UnsupportedLayout => Outcome::Failure(FailureCode::Unsupported),
            Self::UnknownDevice | Self::Unavailable | Self::AdmissionDenied => {
                Outcome::Failure(FailureCode::Unavailable)
            }
            Self::DeviceLost => Outcome::Failure(FailureCode::Loss),
        }
    }
}

impl std::fmt::Display for DeviceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}

impl std::error::Error for DeviceError {}

/// Why the control side could not hand a block to the ring.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PushError {
    /// Not enough free space for the whole block; the caller keeps the block (no silent drop).
    Full,
    /// The block length is not exactly `block_frames * channels` samples.
    WrongLength,
    /// The device was lost; further blocks can never be consumed.
    DeviceLost,
}

impl PushError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::WrongLength => "wrong_length",
            Self::DeviceLost => "device_lost",
        }
    }
}

impl std::fmt::Display for PushError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}

impl std::error::Error for PushError {}
