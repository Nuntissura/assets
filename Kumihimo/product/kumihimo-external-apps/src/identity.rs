use crate::{Capabilities, Diagnostic, Placement, PixelBounds};

/// Live-only selection. Callers must obtain it from an explicitly selected instance.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowTarget {
    pub hwnd: u64,
    pub pid: u32,
    pub expected_creation_time: Option<u64>,
}

/// Deliberately has no serialization implementation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowIdentity {
    pub hwnd: u64,
    pub pid: u32,
    pub creation_time: u64,
}

/// Implementations retain the process lifetime lease and revalidate before every effect.
/// Requests and observations are distinct: successful dispatch is never placement proof.
pub trait WindowBackend {
    type Lease;
    fn bind(&mut self, target: WindowTarget) -> Result<(Self::Lease, WindowIdentity), Diagnostic>;
    fn validate(&mut self, lease: &Self::Lease, identity: WindowIdentity) -> Result<(), Diagnostic>;
    fn observe(&mut self, lease: &Self::Lease) -> Result<Placement, Diagnostic>;
    fn capabilities(&mut self, lease: &Self::Lease) -> Result<Capabilities, Diagnostic>;
    fn request_bounds(&mut self, lease: &Self::Lease, bounds: PixelBounds) -> Result<(), Diagnostic>;
    fn request_restore(&mut self, lease: &Self::Lease, placement: Placement) -> Result<(), Diagnostic>;
    /// True only when the last request has finished, including partial/rejected effects.
    /// Asynchronous backends must keep false until an independent completion witness exists.
    fn request_completed(&mut self, _lease: &Self::Lease) -> Result<bool, Diagnostic> { Ok(false) }
    fn observation_pause(&mut self, milliseconds: u64);
}

pub struct SelectedWindow<L> {
    pub(crate) lease: L,
    pub(crate) identity: WindowIdentity,
    pub(crate) semantic_reference: String,
    pub(crate) original: Placement,
    pub(crate) pending: std::cell::Cell<Option<Placement>>,
}
impl<L> SelectedWindow<L> {
    pub fn identity(&self) -> WindowIdentity { self.identity }
    pub fn semantic_reference(&self) -> &str { &self.semantic_reference }
    pub fn original(&self) -> Placement { self.original }
    pub fn pending(&self) -> Option<Placement> { self.pending.get() }
    /// Native adapters may borrow a lease; they still owe validation before every effect.
    pub fn lease(&self) -> &L { &self.lease }
}
