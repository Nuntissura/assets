//! The typed host event stream: everything past this crate sees these, never a winit type, so a raw Win32 backend can
//! replace winit by editing this crate alone (decision `window_layer`).

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum HostEvent {
    /// Client size in physical pixels; zero when minimised.
    Resized { width: u32, height: u32 },
    /// DPI scale as a thousandth (1000 = 96 dpi).
    ScaleFactor { milli: u32 },
    /// The window gained or lost keyboard focus.
    Focus { gained: bool },
    /// The window became fully hidden or visible again.
    Occluded { hidden: bool },
    CloseRequested,
    RedrawRequested,
    Destroyed,
}

impl HostEvent {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Resized { .. } => "resized",
            Self::ScaleFactor { .. } => "scale_factor",
            Self::Focus { .. } => "focus",
            Self::Occluded { .. } => "occluded",
            Self::CloseRequested => "close_requested",
            Self::RedrawRequested => "redraw_requested",
            Self::Destroyed => "destroyed",
        }
    }
}

/// Events kept per run; later ones are counted.
pub const MAX_EVENTS: usize = 1024;

/// A bounded log of host events.
#[derive(Clone, Debug, Default)]
pub struct EventLog {
    events: Vec<HostEvent>,
    dropped: u64,
}

impl EventLog {
    pub fn push(&mut self, event: HostEvent) {
        if self.events.len() < MAX_EVENTS {
            self.events.push(event);
        } else {
            self.dropped = self.dropped.saturating_add(1);
        }
    }
    pub fn events(&self) -> &[HostEvent] {
        &self.events
    }
    pub fn dropped(&self) -> u64 {
        self.dropped
    }
    pub fn count(&self, code: &str) -> usize {
        self.events.iter().filter(|e| e.code() == code).count()
    }
}

pub(crate) fn from_winit(event: &winit::event::WindowEvent) -> Option<HostEvent> {
    use winit::event::WindowEvent as W;
    Some(match event {
        W::Resized(size) => HostEvent::Resized { width: size.width, height: size.height },
        W::ScaleFactorChanged { scale_factor, .. } => {
            HostEvent::ScaleFactor { milli: (scale_factor * 1000.0).round().clamp(0.0, f64::from(u32::MAX)) as u32 }
        }
        W::Focused(gained) => HostEvent::Focus { gained: *gained },
        W::Occluded(hidden) => HostEvent::Occluded { hidden: *hidden },
        W::CloseRequested => HostEvent::CloseRequested,
        W::RedrawRequested => HostEvent::RedrawRequested,
        W::Destroyed => HostEvent::Destroyed,
        _ => return None,
    })
}
