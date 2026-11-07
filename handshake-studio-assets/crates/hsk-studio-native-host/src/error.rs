//! Typed host outcomes: stable codes, no text from the OS or the driver.
use hsk_studio_vulkan::Refusal;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostError {
    /// The Vulkan owner or presenter refused.
    Vulkan(Refusal),
    /// Bad command line (exit 2; never a window).
    Usage(&'static str),
    /// A file could not be written (`png`, `receipt`).
    Io(&'static str),
    /// The window layer failed (`event_loop`, `create_window`, `handle`, `quiet_style`, `foreign_window`).
    Window(&'static str),
    /// The document could not be shown (`feature_off`, `open`, `render`, `frame`).
    Document(&'static str),
    /// The run passed its frame or wall-clock bound.
    Timeout,
    /// A quiet run took focus or the foreground (HBR-QUIET).
    FocusViolation,
}

impl HostError {
    pub fn code(self) -> &'static str {
        match self {
            Self::Vulkan(r) => r.code(),
            Self::Usage(_) => "usage",
            Self::Io(_) => "io",
            Self::Window(_) => "window",
            Self::Document(_) => "document",
            Self::Timeout => "timeout",
            Self::FocusViolation => "focus_violation",
        }
    }
    /// Second-level code (`detail` in receipts).
    pub fn detail(self) -> &'static str {
        match self {
            Self::Vulkan(r) => r.class(),
            Self::Usage(d) | Self::Io(d) | Self::Window(d) | Self::Document(d) => d,
            Self::Timeout => "bound",
            Self::FocusViolation => "quiet",
        }
    }
    /// Process exit status of `studio-vk`.
    pub fn exit_code(self) -> i32 {
        match self {
            Self::Usage(_) => 2,
            Self::Vulkan(_) => 3,
            Self::Io(_) => 4,
            Self::FocusViolation => 5,
            Self::Timeout => 6,
            Self::Document(_) => 7,
            Self::Window(_) => 8,
        }
    }
}

impl From<Refusal> for HostError {
    fn from(r: Refusal) -> Self {
        Self::Vulkan(r)
    }
}

impl std::fmt::Display for HostError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.code(), self.detail())
    }
}

impl std::error::Error for HostError {}
