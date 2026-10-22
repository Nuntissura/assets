//! Effective evaluation time. Accord `Ticks` are stored as `u64`; the tick a property is *sampled*
//! at may be negative after time remap or posterize (MOT-256 oracle), so sampling uses `EvalTick`.
use crate::error::MotionError;
use hsk_studio_accord::Ticks;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EvalTick(i64);

impl EvalTick {
    pub const fn new(value: i64) -> Self {
        Self(value)
    }

    pub const fn value(self) -> i64 {
        self.0
    }

    pub fn from_stored(ticks: Ticks) -> Result<Self, MotionError> {
        i64::try_from(ticks.value())
            .map(Self)
            .map_err(|_| MotionError::TickOverflow)
    }
}
