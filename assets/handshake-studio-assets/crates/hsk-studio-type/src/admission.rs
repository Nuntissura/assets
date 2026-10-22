use crate::Error;

pub trait EpochPort: Send + Sync {
    fn current_epoch(&self) -> Result<u64, Error>;
}
pub trait RetirementPort: Send + Sync {
    fn retire(&self, handle: u64, requested_bytes: u64);
}
/// Allocation-free ownership of an exact caller reservation. No reduction/mutation escape.
pub struct Reservation<'a> {
    port: &'a dyn RetirementPort,
    handle: u64,
    requested_bytes: u64,
}
impl<'a> Reservation<'a> {
    pub fn new(
        port: &'a dyn RetirementPort,
        handle: u64,
        requested_bytes: u64,
    ) -> Result<Self, Error> {
        if handle == 0 || requested_bytes == 0 {
            return Err(Error::LeaseUnavailable);
        }
        Ok(Self {
            port,
            handle,
            requested_bytes,
        })
    }
    pub fn requested_bytes(&self) -> u64 {
        self.requested_bytes
    }
}
impl Drop for Reservation<'_> {
    fn drop(&mut self) {
        self.port.retire(self.handle, self.requested_bytes);
    }
}
/// The provider checks the returned reservation equals its exact requested Layout.
pub trait AdmissionPort: Send + Sync {
    fn reserve(
        &self,
        requested_bytes: u64,
        simultaneous_limit: u64,
    ) -> Result<Reservation<'_>, Error>;
    /// This port is scoped to one operation; historical high-water is not this operation's peak.
    fn snapshot(&self) -> Result<AllocationSnapshot, Error>;
    fn retain_generation(&self, simultaneous_limit: u64) -> Result<Generation<'_>, Error>;
}
pub trait GenerationRetirement: Send + Sync {
    fn retire_generation(&self, handle: u64);
}
/// Unique ownership of one caller-admitted retained result generation.
pub struct Generation<'a> {
    port: &'a dyn GenerationRetirement,
    handle: u64,
}
impl<'a> Generation<'a> {
    pub fn new(port: &'a dyn GenerationRetirement, handle: u64) -> Result<Self, Error> {
        if handle == 0 {
            return Err(Error::LeaseUnavailable);
        }
        Ok(Self { port, handle })
    }
}
impl Drop for Generation<'_> {
    fn drop(&mut self) {
        self.port.retire_generation(self.handle);
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AllocationSnapshot {
    pub current_requested_bytes: u64,
    pub operation_peak_requested_bytes: u64,
    pub historical_peak_requested_bytes: u64,
    pub retained_generations: u64,
}
