//! Spring easing and directional transitions (lane SMB-FIG-SMART-ANIMATE-2; STU-PRO-116..121).
//!
//! Spring (STU-PRO-119 `easing_function_spring {mass, stiffness, damping, initial_velocity}`): the
//! eased progress is `1 - x(t)` of the analytic damped harmonic oscillator `m x'' + c x' + k x = 0`
//! with `x(0) = 1`, `x'(0) = -initial_velocity` (a positive velocity moves toward the target), `t` in
//! seconds of the running transition. With `w0 = sqrt(k / m)` and `zeta = c / (2 sqrt(k m))`:
//! under-damped `x = e^(-zeta w0 t) (cos(wd t) + (zeta w0 - v0) / wd sin(wd t))`, `wd = w0
//! sqrt(1 - zeta^2)`; critically damped (`|zeta - 1| <= 1e-9`) `x = (1 + (w0 - v0) t) e^(-w0 t)`;
//! over-damped `x = C1 e^(r1 t) + C2 e^(r2 t)`, `r1,2 = -w0 (zeta -/+ sqrt(zeta^2 - 1))`,
//! `C1 = (-v0 - r2) / (r1 - r2)`, `C2 = 1 - C1`. This is the solution the WebKit SpringSolver
//! evaluates (Figma's blog names that solver; WebKit approximates the over-damped case by the
//! critical one, Studio solves it exactly). Figma's own numbers and its preset parameter sets are
//! UNVERIFIED (oracle D5). The transition ends at its stored duration (progress then 1).
//!
//! Directional transitions (STU-PRO-118, Studio reading, Figma parity UNVERIFIED): `direction` is
//! the edge the incoming screen enters from (`MOVE_IN`, `PUSH`, `SLIDE_IN`) or the edge the outgoing
//! screen leaves toward (`MOVE_OUT`, `SLIDE_OUT`), matching the `.fig` tokens `MOVE_FROM_LEFT`,
//! `SLIDE_OUT_TO_RIGHT`, ... With the edge vector `d` (LEFT (-1, 0), RIGHT (1, 0), TOP (0, -1),
//! BOTTOM (0, 1)) scaled by the screen size and the eased progress `e`:
//! `MOVE_IN` to = d (1 - e), from fixed, to on top; `PUSH` to = d (1 - e), from = -d e;
//! `SLIDE_IN` to = d (1 - e), from = -d e [`SLIDE_PARALLAX`], to on top; `MOVE_OUT` from = d e, to
//! fixed, from on top; `SLIDE_OUT` from = d e, to = -d (1 - e) [`SLIDE_PARALLAX`], from on top.
use hsk_studio_folio::{SpringEasing, TransitionDirection, TransitionType};

/// Share of the screen size the covered screen travels in `SLIDE_IN` / `SLIDE_OUT` (Studio-declared).
pub const SLIDE_PARALLAX: f64 = 0.3;
/// Longest settling time [`spring_settle_ms`] searches (the STU-PRO-121 duration hard maximum).
pub const MAX_SPRING_SETTLE_MS: u32 = 30_000;

/// Displacement `x(t)` of the oscillator (1 at `t = 0`, 0 at rest). `None` for a non-physical
/// parameter set (mass or stiffness not > 0, damping < 0, any value not finite).
pub fn spring_displacement(s: &SpringEasing, t: f64) -> Option<f64> {
    let (m, k, c, v0) = (s.mass, s.stiffness, s.damping, s.initial_velocity);
    if ![m, k, c, v0, t].iter().all(|v| v.is_finite()) || m <= 0.0 || k <= 0.0 || c < 0.0 {
        return None;
    }
    let t = t.max(0.0);
    let w0 = (k / m).sqrt();
    let zeta = c / (2.0 * (k * m).sqrt());
    let x = if (zeta - 1.0).abs() <= 1e-9 {
        (1.0 + (w0 - v0) * t) * (-w0 * t).exp()
    } else if zeta < 1.0 {
        let wd = w0 * (1.0 - zeta * zeta).sqrt();
        let b = (zeta * w0 - v0) / wd;
        (-zeta * w0 * t).exp() * ((wd * t).cos() + b * (wd * t).sin())
    } else {
        let s = (zeta * zeta - 1.0).sqrt();
        let (r1, r2) = (-w0 * (zeta - s), -w0 * (zeta + s));
        let c1 = (-v0 - r2) / (r1 - r2);
        c1 * (r1 * t).exp() + (1.0 - c1) * (r2 * t).exp()
    };
    x.is_finite().then_some(x)
}

/// Eased progress `1 - x(t)` of a spring at `t` seconds (overshoots above 1 when under-damped).
pub fn spring_progress(s: &SpringEasing, t: f64) -> Option<f64> {
    spring_displacement(s, t).map(|x| 1.0 - x)
}

/// The first whole millisecond after which `|x|` stays at or below `tolerance` up to
/// [`MAX_SPRING_SETTLE_MS`]; `None` when it does not settle within that bound (or bad parameters).
pub fn spring_settle_ms(s: &SpringEasing, tolerance: f64) -> Option<u32> {
    if !(tolerance.is_finite() && tolerance > 0.0) {
        return None;
    }
    if spring_displacement(s, f64::from(MAX_SPRING_SETTLE_MS) / 1000.0)?.abs() > tolerance {
        return None;
    }
    let mut ms = MAX_SPRING_SETTLE_MS;
    while ms > 0 {
        let prev = ms.saturating_sub(1);
        if spring_displacement(s, f64::from(prev) / 1000.0)?.abs() > tolerance {
            return Some(ms);
        }
        ms = prev;
    }
    Some(0)
}

/// Where the two screens of a directional transition are at eased progress `e`, as offsets in px
/// from their rest position (screen size `w` x `h`), and which one is drawn on top.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DirectionalFrame {
    pub from_offset: (f64, f64),
    pub to_offset: (f64, f64),
    pub to_on_top: bool,
}

/// The STU-PRO-118 directional placement (module rule). `None` for a non-directional type.
pub fn directional_frame(kind: TransitionType, direction: TransitionDirection, e: f64, w: f64, h: f64) -> Option<DirectionalFrame> {
    let (dx, dy) = match direction {
        TransitionDirection::Left => (-w, 0.0),
        TransitionDirection::Right => (w, 0.0),
        TransitionDirection::Top => (0.0, -h),
        TransitionDirection::Bottom => (0.0, h),
    };
    let at = |k: f64| (dx * k, dy * k);
    let (from_offset, to_offset, to_on_top) = match kind {
        TransitionType::MoveIn => ((0.0, 0.0), at(1.0 - e), true),
        TransitionType::Push => (at(-e), at(1.0 - e), true),
        TransitionType::SlideIn => (at(-e * SLIDE_PARALLAX), at(1.0 - e), true),
        TransitionType::MoveOut => (at(e), (0.0, 0.0), false),
        TransitionType::SlideOut => (at(e), at(-(1.0 - e) * SLIDE_PARALLAX), false),
        TransitionType::Dissolve | TransitionType::SmartAnimate | TransitionType::ScrollAnimate => return None,
    };
    // -0.0 from a zero product reads as 0 in comparisons; normalise it for exact equality tests.
    let n = |(x, y): (f64, f64)| (x + 0.0, y + 0.0);
    Some(DirectionalFrame { from_offset: n(from_offset), to_offset: n(to_offset), to_on_top })
}
