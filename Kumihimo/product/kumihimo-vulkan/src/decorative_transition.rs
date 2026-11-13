use std::time::{Duration, Instant};

struct ActiveTransition {
    started: Instant,
    duration: Duration,
}

#[derive(Default)]
pub(super) struct DecorativeTransition {
    reduced: bool,
    active: Option<ActiveTransition>,
}

impl DecorativeTransition {
    /// Returns whether this change needs a redraw to show the committed destination.
    pub(super) fn set_reduced_motion(&mut self, reduced: bool) -> bool {
        if self.reduced == reduced {
            return false;
        }
        self.reduced = reduced;
        reduced && self.active.take().is_some()
    }

    pub(super) fn start(&mut self, now: Instant, duration: Duration) {
        self.active = (!self.reduced && !duration.is_zero()).then_some(ActiveTransition {
            started: now,
            duration,
        });
    }

    /// Returns progress and the next decorative redraw, capped at the end of the transition.
    pub(super) fn sample(&mut self, now: Instant, next_frame: Instant) -> (f64, Option<Instant>) {
        if let Some(active) = &self.active {
            let elapsed = now.saturating_duration_since(active.started);
            if elapsed < active.duration {
                let deadline = active.started.checked_add(active.duration);
                let next = deadline.map_or(next_frame, |end| end.min(next_frame));
                return (
                    elapsed.as_secs_f64() / active.duration.as_secs_f64(),
                    Some(next),
                );
            }
        }
        self.active = None;
        (1.0, None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DURATION: Duration = Duration::from_millis(150);

    #[test]
    fn normal_motion_uses_the_supplied_duration() {
        let start = Instant::now();
        let mut motion = DecorativeTransition::default();
        motion.start(start, DURATION);
        let middle = start + DURATION / 2;
        let end = start + DURATION;

        assert_eq!(motion.sample(middle, end + DURATION), (0.5, Some(end)));
        assert_eq!(motion.sample(end, end + DURATION), (1.0, None));
    }

    #[test]
    fn reduction_settles_once_and_resume_requires_a_new_transition() {
        let start = Instant::now();
        let next = start + DURATION / 2;
        let mut motion = DecorativeTransition::default();
        motion.start(start, DURATION);

        assert!(motion.set_reduced_motion(true));
        assert_eq!(motion.sample(start, next), (1.0, None));
        assert!(!motion.set_reduced_motion(true));
        motion.start(start, DURATION);
        assert_eq!(motion.sample(start, next), (1.0, None));

        assert!(!motion.set_reduced_motion(false));
        assert_eq!(motion.sample(start, next), (1.0, None));
        motion.start(start, DURATION);
        assert_eq!(motion.sample(start, next), (0.0, Some(next)));
    }

    #[test]
    fn one_surface_can_settle_while_another_continues() {
        let start = Instant::now();
        let next = start + DURATION / 2;
        let mut root = DecorativeTransition::default();
        let mut detached = DecorativeTransition::default();
        root.start(start, DURATION);
        detached.start(start, DURATION);

        assert!(root.set_reduced_motion(true));
        assert_eq!(root.sample(start, next), (1.0, None));
        assert_eq!(detached.sample(start, next), (0.0, Some(next)));
    }
}
