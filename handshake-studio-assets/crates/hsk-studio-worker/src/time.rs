//! Overflow-safe deadline arithmetic for supervision waits.
#![deny(clippy::arithmetic_side_effects)]
use std::time::{Duration, Instant};

/// Longest wait this crate will compute an `Instant` for. A caller-supplied duration above it is
/// clamped, so `Instant + Duration` cannot overflow (it panics in std) whatever a limit says
/// (test: `tests::deadline_arithmetic_never_overflows`).
pub const WAIT_CEILING: Duration = Duration::from_secs(24 * 60 * 60);

/// `now + wait`, clamped to [`WAIT_CEILING`]. If even that cannot be represented the deadline is
/// `now`, so a wait ends at once instead of panicking or never ending.
pub fn after(wait: Duration) -> Instant {
    let now = Instant::now();
    now.checked_add(wait.min(WAIT_CEILING)).unwrap_or(now)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deadline_arithmetic_never_overflows() {
        let start = Instant::now();
        for wait in [
            Duration::MAX,
            Duration::from_secs(u64::MAX),
            Duration::from_secs(u64::MAX / 2),
            WAIT_CEILING,
        ] {
            let at = after(wait);
            assert!(at >= start, "{wait:?}");
            assert!(
                at <= Instant::now() + WAIT_CEILING + Duration::from_secs(1),
                "{wait:?} was not clamped"
            );
        }
        let soon = after(Duration::from_millis(250));
        assert!(soon >= start + Duration::from_millis(250));
        assert!(soon < start + Duration::from_secs(60));
    }
}
