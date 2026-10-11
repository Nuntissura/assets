/// Stable, content-free failure vocabulary. `code()` strings are wire-stable snake_case.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScoreError {
    /// Sample rate is zero, above the authored bound, not an exact tick divisor, or differs from a rate-bound processor.
    InvalidRate,
    /// Channel layout is empty, too wide or carries duplicate labels.
    InvalidLayout,
    /// Channel count or labels do not match the processor, mix matrix or output block.
    ChannelMismatch,
    /// A channel slice length differs from the declared frame count.
    LengthMismatch,
    /// Requested frames exceed the preallocated output capacity or a fixed bound.
    CapacityExceeded,
    /// Sample position arithmetic would overflow.
    PositionOverflow,
    /// The single admitted block slot is already reserved.
    Busy,
    /// Cancellation was observed; nothing was published.
    Canceled,
    /// Expected revision differs from the engine revision at admission or at publication.
    StaleRevision,
    /// The request carried no actor grant context.
    MissingContext,
    /// A sample or parameter is NaN or infinite.
    NotFinite,
    /// A parameter, frame count or configuration value is outside its declared range.
    OutOfRange,
    /// The request names a resource other than the one the engine slot is bound to.
    ResourceMismatch,
    /// The resampling backend rejected construction or processing.
    ResampleFailed,
    /// Blocks that must be summed disagree on sample rate, start sample or frame count.
    TimelineMismatch,
}

impl ScoreError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::InvalidRate => "invalid_rate",
            Self::InvalidLayout => "invalid_layout",
            Self::ChannelMismatch => "channel_mismatch",
            Self::LengthMismatch => "length_mismatch",
            Self::CapacityExceeded => "capacity_exceeded",
            Self::PositionOverflow => "position_overflow",
            Self::Busy => "busy",
            Self::Canceled => "canceled",
            Self::StaleRevision => "stale_revision",
            Self::MissingContext => "missing_context",
            Self::NotFinite => "not_finite",
            Self::OutOfRange => "out_of_range",
            Self::ResourceMismatch => "resource_mismatch",
            Self::ResampleFailed => "resample_failed",
            Self::TimelineMismatch => "timeline_mismatch",
        }
    }
}

impl std::fmt::Display for ScoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}

impl std::error::Error for ScoreError {}
