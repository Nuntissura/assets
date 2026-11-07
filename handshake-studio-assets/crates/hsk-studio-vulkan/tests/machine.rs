//! Swapchain state machine with injected faults (no device): every Vulkan result the presenter can see takes its
//! typed path.
use hsk_studio_vulkan::machine::{Action, MAX_LOG, MAX_SURFACE_ATTEMPTS, Machine, Signal, SwapState, step};

#[test]
fn out_of_date_and_suboptimal_recreate_then_run() {
    let mut m = Machine::default();
    assert_eq!(m.feed(Signal::Created), Action::Draw);
    assert_eq!(m.state(), SwapState::Running);
    for fault in [Signal::OutOfDate, Signal::Suboptimal, Signal::Resized] {
        assert_eq!(m.feed(fault), Action::Recreate, "{fault:?}");
        assert_eq!(m.state(), SwapState::Stale);
        assert_eq!(m.feed(Signal::Acquired), Action::Recreate, "stale never draws");
        assert_eq!(m.feed(Signal::Created), Action::Draw);
    }
    assert_eq!(m.feed(Signal::AcquireTimeout), Action::Skip);
    assert_eq!(m.state(), SwapState::Running);
}

#[test]
fn minimise_waits_without_acquire_or_create() {
    let mut m = Machine::default();
    m.feed(Signal::Created);
    assert_eq!(m.feed(Signal::ZeroExtent), Action::WaitForSize);
    for s in [Signal::Acquired, Signal::Created, Signal::Presented, Signal::AcquireTimeout] {
        assert_eq!(m.feed(s), Action::WaitForSize, "{s:?}");
        assert_eq!(m.state(), SwapState::Minimized);
    }
    assert_eq!(m.feed(Signal::Restored), Action::Recreate);
    assert_eq!(m.state(), SwapState::Stale);
    // A zero extent seen while re-creating goes back to minimised.
    assert_eq!(m.feed(Signal::ZeroExtent), Action::WaitForSize);
    assert_eq!(m.feed(Signal::Resized), Action::Recreate);
}

#[test]
fn surface_loss_is_bounded_then_unrecoverable() {
    let mut m = Machine::default();
    m.feed(Signal::Created);
    assert_eq!(m.feed(Signal::SurfaceLost), Action::RecreateSurface);
    assert_eq!(m.state(), SwapState::SurfaceLost { attempts: 1 });
    assert_eq!(m.feed(Signal::SurfaceRecreated), Action::Recreate);
    assert_eq!(m.feed(Signal::Created), Action::Draw);
    assert_eq!(m.surface_losses(), 1);
    m.feed(Signal::Presented);
    assert_eq!(m.surface_losses(), 0, "a presented frame resets the loss count");
    // Lost again and again: never a plain swapchain re-create loop.
    m.feed(Signal::SurfaceLost);
    for _ in 1..MAX_SURFACE_ATTEMPTS {
        assert_eq!(m.feed(Signal::SurfaceLost), Action::RecreateSurface);
    }
    assert_eq!(m.state(), SwapState::SurfaceLost { attempts: MAX_SURFACE_ATTEMPTS });
    assert_eq!(m.feed(Signal::SurfaceLost), Action::Stop);
    assert_eq!(m.state(), SwapState::SurfaceUnrecoverable);
    assert_eq!(m.feed(Signal::Created), Action::Stop, "terminal absorbs");
}

#[test]
fn device_loss_is_terminal_from_every_state() {
    for start in [SwapState::Uncreated, SwapState::Running, SwapState::Stale, SwapState::Minimized, SwapState::SurfaceLost { attempts: 2 }] {
        assert_eq!(step(start, Signal::DeviceLost), (SwapState::DeviceLost, Action::Rebind), "{start:?}");
    }
    for s in [Signal::Created, Signal::Restored, Signal::SurfaceRecreated] {
        assert_eq!(step(SwapState::DeviceLost, s), (SwapState::DeviceLost, Action::Stop));
    }
    assert!(SwapState::DeviceLost.terminal() && SwapState::SurfaceUnrecoverable.terminal());
    assert!(!SwapState::Minimized.terminal());
}

#[test]
fn log_keeps_changes_not_routine_frames_and_is_bounded() {
    let mut m = Machine::default();
    m.feed(Signal::Created);
    for _ in 0..100 {
        m.feed(Signal::Acquired);
        m.feed(Signal::Presented);
    }
    assert_eq!(m.log().len(), 1, "routine frames are not logged");
    for _ in 0..(MAX_LOG * 2) {
        m.feed(Signal::OutOfDate);
        m.feed(Signal::Created);
    }
    assert_eq!(m.log().len(), MAX_LOG);
    assert!(m.dropped() > 0);
    let codes: Vec<&str> = [SwapState::Uncreated, SwapState::Running, SwapState::Stale, SwapState::Minimized]
        .iter()
        .map(|s| s.code())
        .collect();
    assert_eq!(codes, ["uncreated", "running", "stale", "minimized"]);
}

#[test]
fn surface_lost_again_after_recovery_still_reaches_unrecoverable() {
    // The surface comes back (create succeeds) but is lost again before any frame is presented, four times.
    let mut m = Machine::default();
    m.feed(Signal::Created);
    for cycle in 1..=MAX_SURFACE_ATTEMPTS {
        assert_eq!(m.feed(Signal::SurfaceLost), Action::RecreateSurface, "cycle {cycle}");
        assert_eq!(m.state(), SwapState::SurfaceLost { attempts: cycle });
        assert_eq!(m.feed(Signal::SurfaceRecreated), Action::Recreate);
        assert_eq!(m.feed(Signal::Created), Action::Draw);
        assert_eq!(m.feed(Signal::Acquired), Action::Draw);
    }
    assert_eq!(m.feed(Signal::SurfaceLost), Action::Stop);
    assert_eq!(m.state(), SwapState::SurfaceUnrecoverable);
    // With a presented frame between losses the count restarts and the surface keeps recovering.
    let mut ok = Machine::default();
    ok.feed(Signal::Created);
    for _ in 0..(u32::from(MAX_SURFACE_ATTEMPTS) * 3) {
        assert_eq!(ok.feed(Signal::SurfaceLost), Action::RecreateSurface);
        ok.feed(Signal::SurfaceRecreated);
        ok.feed(Signal::Created);
        ok.feed(Signal::Presented);
    }
    assert_eq!(ok.state(), SwapState::Running);
}
