//! The typed host event stream: everything past this crate sees these, never a winit type, so a raw Win32 backend can
//! replace winit by editing this crate alone (decision `window_layer`). Positions are physical client pixels as i32,
//! so every event stays `Copy` and `Eq`; a scripted list of these is the only input a test or agent ever feeds
//! (STU-QUIET-002: never OS input).

/// Modifier keys held.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Mods {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Button {
    Left,
    Right,
    Middle,
    Other,
}

/// The keys the host routes; everything else is `Other`. `Char` is a lowercase ASCII letter or digit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Key {
    Char(char),
    Tab,
    Enter,
    Escape,
    Space,
    Up,
    Down,
    PageUp,
    PageDown,
    Home,
    End,
    /// Lane SMB-GUI-MENU-1 (components.json menu_popup, menu_bar, context_menu keyboard): Left and Right step titles
    /// and open or close cascades, F10 reaches the menu bar, the context-menu key opens a context menu.
    Left,
    Right,
    F10,
    ContextMenu,
    Other,
}

impl Key {
    /// A key by its script name (`z`, `y`, `tab`, `pageup`, ...).
    pub fn from_name(name: &str) -> Option<Key> {
        Some(match name {
            "tab" => Key::Tab,
            "enter" => Key::Enter,
            "escape" | "esc" => Key::Escape,
            "space" => Key::Space,
            "up" => Key::Up,
            "down" => Key::Down,
            "pageup" => Key::PageUp,
            "pagedown" => Key::PageDown,
            "home" => Key::Home,
            "end" => Key::End,
            "left" => Key::Left,
            "right" => Key::Right,
            "f10" => Key::F10,
            "menu" => Key::ContextMenu,
            s if s.len() == 1 && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()) => Key::Char(char::from(s.as_bytes()[0])),
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
    /// Pointer position in physical client pixels (may be outside the client area while a button is held).
    PointerMoved { x: i32, y: i32 },
    /// The pointer left the client area.
    PointerLeft,
    Button { button: Button, pressed: bool },
    /// Wheel delta: thousandths of a line, or physical pixels when `pixels`. Positive `y` reveals content above.
    Wheel { x: i32, y: i32, pixels: bool },
    Key { key: Key, pressed: bool, mods: Mods },
    Modifiers(Mods),
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
            Self::PointerMoved { .. } => "pointer_moved",
            Self::PointerLeft => "pointer_left",
            Self::Button { .. } => "button",
            Self::Wheel { .. } => "wheel",
            Self::Key { .. } => "key",
            Self::Modifiers(_) => "modifiers",
        }
    }
}

/// Events kept per run; later ones are counted. Pointer moves are coalesced and never logged.
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

fn px(v: f64) -> i32 {
    // `as` saturates and maps NaN to 0.
    v.round() as i32
}

/// Maps one winit window event; `mods` is the modifier state the caller tracks from `ModifiersChanged`.
pub(crate) fn from_winit(event: &winit::event::WindowEvent, mods: Mods) -> Option<HostEvent> {
    use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent as W};
    use winit::keyboard::{Key as K, NamedKey as N};
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
        W::CursorMoved { position, .. } => HostEvent::PointerMoved { x: px(position.x), y: px(position.y) },
        W::CursorLeft { .. } => HostEvent::PointerLeft,
        W::MouseInput { state, button, .. } => HostEvent::Button {
            button: match button {
                MouseButton::Left => Button::Left,
                MouseButton::Right => Button::Right,
                MouseButton::Middle => Button::Middle,
                _ => Button::Other,
            },
            pressed: *state == ElementState::Pressed,
        },
        W::MouseWheel { delta, .. } => match delta {
            MouseScrollDelta::LineDelta(x, y) => {
                HostEvent::Wheel { x: px(f64::from(*x) * 1000.0), y: px(f64::from(*y) * 1000.0), pixels: false }
            }
            MouseScrollDelta::PixelDelta(p) => HostEvent::Wheel { x: px(p.x), y: px(p.y), pixels: true },
        },
        W::ModifiersChanged(m) => {
            let s = m.state();
            HostEvent::Modifiers(Mods { ctrl: s.control_key(), shift: s.shift_key(), alt: s.alt_key() })
        }
        W::KeyboardInput { event, .. } => {
            // `logical_key` carries every modifier except Ctrl (winit 0.30 KeyEvent docs), so Ctrl+Shift+Z is "Z".
            let key = match &event.logical_key {
                K::Named(N::Tab) => Key::Tab,
                K::Named(N::Enter) => Key::Enter,
                K::Named(N::Escape) => Key::Escape,
                K::Named(N::Space) => Key::Space,
                K::Named(N::ArrowUp) => Key::Up,
                K::Named(N::ArrowDown) => Key::Down,
                K::Named(N::PageUp) => Key::PageUp,
                K::Named(N::PageDown) => Key::PageDown,
                K::Named(N::Home) => Key::Home,
                K::Named(N::End) => Key::End,
                K::Named(N::ArrowLeft) => Key::Left,
                K::Named(N::ArrowRight) => Key::Right,
                K::Named(N::F10) => Key::F10,
                K::Named(N::ContextMenu) => Key::ContextMenu,
                K::Character(s) => {
                    let mut chars = s.chars();
                    match (chars.next(), chars.next()) {
                        (Some(c), None) if c.is_ascii_alphanumeric() => Key::Char(c.to_ascii_lowercase()),
                        _ => Key::Other,
                    }
                }
                _ => Key::Other,
            };
            HostEvent::Key { key, pressed: event.state == ElementState::Pressed, mods }
        }
        _ => return None,
    })
}
