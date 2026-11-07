//! The swapchain state machine (GUI-DESKTOP-022, 036, 037): plain data, no Vulkan type. The presenter feeds every real
//! Vulkan result into [`step`] and does what the returned [`Action`] says, so tests drive the same machine with
//! synthetic signals (injected faults) and no device.

/// Bounded surface re-creation attempts before `surface_unrecoverable` (a surface loss treated as a swapchain
/// re-create is the known infinite-loop defect; SDL and DXVK both bound it).
pub const MAX_SURFACE_ATTEMPTS: u8 = 3;

/// One window's swapchain state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SwapState {
    /// No swapchain yet.
    Uncreated,
    /// Draws and presents.
    Running,
    /// Out of date or suboptimal: re-create with `oldSwapchain` before the next draw.
    Stale,
    /// Zero extent: no acquire and no create until the window has a size again.
    Minimized,
    /// Surface lost: destroy swapchain and surface, re-create both from the same window, `attempts` so far.
    SurfaceLost { attempts: u8 },
    /// Device lost: the owner must rebind (generation + 1). Terminal in this lane.
    DeviceLost,
    /// The surface did not come back within [`MAX_SURFACE_ATTEMPTS`]. Terminal.
    SurfaceUnrecoverable,
}

impl SwapState {
    /// Stable code for receipts and observe frames.
    pub const fn code(self) -> &'static str {
        match self {
            Self::Uncreated => "uncreated",
            Self::Running => "running",
            Self::Stale => "stale",
            Self::Minimized => "minimized",
            Self::SurfaceLost { .. } => "surface_lost",
            Self::DeviceLost => "device_lost",
            Self::SurfaceUnrecoverable => "surface_unrecoverable",
        }
    }
    /// True for states nothing leaves.
    pub const fn terminal(self) -> bool {
        matches!(self, Self::DeviceLost | Self::SurfaceUnrecoverable)
    }
}

/// What happened, as the presenter or the window saw it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Signal {
    /// A swapchain was created with a non-zero extent.
    Created,
    /// The surface reports a zero extent (minimised).
    ZeroExtent,
    /// An image was acquired.
    Acquired,
    /// A frame was presented.
    Presented,
    /// `VK_ERROR_OUT_OF_DATE_KHR`.
    OutOfDate,
    /// `VK_SUBOPTIMAL_KHR`.
    Suboptimal,
    /// The window was resized to a non-zero size.
    Resized,
    /// The window was restored from minimised.
    Restored,
    /// `VK_ERROR_SURFACE_LOST_KHR`.
    SurfaceLost,
    /// A lost surface was re-created.
    SurfaceRecreated,
    /// `VK_ERROR_DEVICE_LOST`.
    DeviceLost,
    /// The bounded acquire wait ran out (`VK_TIMEOUT` or `VK_NOT_READY`): skip this frame.
    AcquireTimeout,
}

impl Signal {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::ZeroExtent => "zero_extent",
            Self::Acquired => "acquired",
            Self::Presented => "presented",
            Self::OutOfDate => "out_of_date",
            Self::Suboptimal => "suboptimal",
            Self::Resized => "resized",
            Self::Restored => "restored",
            Self::SurfaceLost => "surface_lost",
            Self::SurfaceRecreated => "surface_recreated",
            Self::DeviceLost => "device_lost",
            Self::AcquireTimeout => "acquire_timeout",
        }
    }
}

/// What the presenter does next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Acquire, draw, present.
    Draw,
    /// Wait for the device to idle, then re-create the swapchain with `oldSwapchain`.
    Recreate,
    /// Do nothing until the window has a size.
    WaitForSize,
    /// Destroy swapchain and surface and re-create both from the same window.
    RecreateSurface,
    /// Skip this frame, keep the state.
    Skip,
    /// The owner must rebind its device; the presenter stops.
    Rebind,
    /// Stop: the state is terminal.
    Stop,
}

impl Action {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Draw => "draw",
            Self::Recreate => "recreate",
            Self::WaitForSize => "wait_for_size",
            Self::RecreateSurface => "recreate_surface",
            Self::Skip => "skip",
            Self::Rebind => "rebind",
            Self::Stop => "stop",
        }
    }
}

/// One transition. Terminal states absorb every signal.
pub fn step(state: SwapState, signal: Signal) -> (SwapState, Action) {
    use Action as A;
    use Signal as S;
    use SwapState as T;
    if state.terminal() {
        return (state, A::Stop);
    }
    match (state, signal) {
        (_, S::DeviceLost) => (T::DeviceLost, A::Rebind),
        (T::SurfaceLost { attempts }, S::SurfaceLost) => {
            let attempts = attempts.saturating_add(1);
            if attempts > MAX_SURFACE_ATTEMPTS {
                (T::SurfaceUnrecoverable, A::Stop)
            } else {
                (T::SurfaceLost { attempts }, A::RecreateSurface)
            }
        }
        (_, S::SurfaceLost) => (T::SurfaceLost { attempts: 1 }, A::RecreateSurface),
        (T::SurfaceLost { .. }, S::SurfaceRecreated) => (T::Stale, A::Recreate),
        (T::SurfaceLost { attempts }, _) => (T::SurfaceLost { attempts }, A::RecreateSurface),
        (_, S::ZeroExtent) => (T::Minimized, A::WaitForSize),
        (T::Minimized, S::Resized | S::Restored) => (T::Stale, A::Recreate),
        (T::Minimized, _) => (T::Minimized, A::WaitForSize),
        (T::Uncreated | T::Stale, S::Created) => (T::Running, A::Draw),
        (T::Uncreated, _) => (T::Uncreated, A::Recreate),
        (T::Stale, _) => (T::Stale, A::Recreate),
        (T::Running, S::OutOfDate | S::Suboptimal | S::Resized | S::Restored) => (T::Stale, A::Recreate),
        (T::Running, S::AcquireTimeout) => (T::Running, A::Skip),
        (T::Running, _) => (T::Running, A::Draw),
        (T::DeviceLost | T::SurfaceUnrecoverable, _) => (state, A::Stop),
    }
}

/// One recorded transition (receipt and observe projection).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Transition {
    pub from: SwapState,
    pub signal: Signal,
    pub to: SwapState,
    pub action: Action,
}

/// The machine with a bounded transition log.
#[derive(Clone, Debug)]
pub struct Machine {
    state: SwapState,
    log: Vec<Transition>,
    dropped: u64,
}

/// Transitions kept per machine; later ones are counted in `dropped`.
pub const MAX_LOG: usize = 512;

impl Default for Machine {
    fn default() -> Self {
        Self { state: SwapState::Uncreated, log: Vec::new(), dropped: 0 }
    }
}

impl Machine {
    pub fn state(&self) -> SwapState {
        self.state
    }
    pub fn log(&self) -> &[Transition] {
        &self.log
    }
    pub fn dropped(&self) -> u64 {
        self.dropped
    }
    /// Applies `signal` and returns the action. Repeated `Acquired`/`Presented` in `Running` are not logged, so the
    /// log holds state changes and faults, not one line per frame.
    pub fn feed(&mut self, signal: Signal) -> Action {
        let (to, action) = step(self.state, signal);
        let routine = to == self.state && matches!(signal, Signal::Acquired | Signal::Presented);
        if !routine {
            if self.log.len() < MAX_LOG {
                self.log.push(Transition { from: self.state, signal, to, action });
            } else {
                self.dropped = self.dropped.saturating_add(1);
            }
        }
        self.state = to;
        action
    }
}
