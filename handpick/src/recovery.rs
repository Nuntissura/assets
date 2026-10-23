/// Maximum simultaneous exclusive recovery owners within one authorized host scope.
pub const RECOVERY_SLOT_COUNT: u8 = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecoverySlotError { InvalidSlot, OutOfOrder, Closed }
impl std::fmt::Display for RecoverySlotError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(f, "{self:?}") }
}
impl std::error::Error for RecoverySlotError {}

/// Effect-free acquisition order. Hosts own exclusive locks, durable namespaces and storage.
/// Only an actual busy-lock outcome may advance this policy; other failures abort acquisition.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecoverySlots {
    order: [u8; RECOVERY_SLOT_COUNT as usize],
    next: usize,
    acquired: Option<u8>,
}
impl RecoverySlots {
    pub fn new(preferred: u8) -> Result<Self, RecoverySlotError> {
        if preferred >= RECOVERY_SLOT_COUNT { return Err(RecoverySlotError::InvalidSlot); }
        let mut order = [0; RECOVERY_SLOT_COUNT as usize];
        order[0] = preferred;
        let mut position = 1;
        for slot in 0..RECOVERY_SLOT_COUNT {
            if slot != preferred { order[position] = slot; position += 1; }
        }
        Ok(Self { order, next: 0, acquired: None })
    }
    /// Next exclusive lock to try; None is terminal acquisition or exhaustion.
    pub fn current(&self) -> Option<u8> {
        if self.acquired.is_some() { None } else { self.order.get(self.next).copied() }
    }
    fn check(&self, slot: u8) -> Result<(), RecoverySlotError> {
        if slot >= RECOVERY_SLOT_COUNT { return Err(RecoverySlotError::InvalidSlot); }
        match self.current() {
            None => Err(RecoverySlotError::Closed),
            Some(current) if current != slot => Err(RecoverySlotError::OutOfOrder),
            Some(_) => Ok(()),
        }
    }
    /// Advance once after the current owner lock reports busy. None means exhausted.
    pub fn busy(&mut self, slot: u8) -> Result<Option<u8>, RecoverySlotError> {
        self.check(slot)?; self.next += 1; Ok(self.current())
    }
    /// Finish after the host acquired the current lock; never transfers or steals ownership.
    pub fn acquired(&mut self, slot: u8) -> Result<(), RecoverySlotError> {
        self.check(slot)?; self.acquired = Some(slot); Ok(())
    }
    pub fn acquired_slot(&self) -> Option<u8> { self.acquired }
    /// Distinguish exhausted acquisition from a successfully acquired terminal policy.
    pub fn exhausted(&self) -> bool { self.acquired.is_none() && self.next == self.order.len() }
}
impl Default for RecoverySlots {
    fn default() -> Self { Self::new(0).expect("legacy slot zero is valid") }
}
