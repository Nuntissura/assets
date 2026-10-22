use crate::{AdmissionPort, EpochPort, Error, Reservation};
use hsk_studio_accord::CancellationToken;
use std::cell::Cell;

pub(crate) type ProviderResult<T> = Result<T, Error>;
/// Invocation-local context; borrowed explicitly, never ambient or stored in a returned Face.
pub(crate) struct Context<'a> {
    cancel: &'a CancellationToken,
    epoch: &'a dyn EpochPort,
    expected_epoch: u64,
    admission: &'a dyn AdmissionPort,
    work_limit: u64,
    byte_limit: u64,
    recursion_limit: u64,
    work: Cell<u64>,
    depth: Cell<u64>,
    glyph_limit: Cell<u64>,
    lookup_limit: Cell<u64>,
    source:Cell<Option<crate::SourceRange>>,
}
impl<'a> Context<'a> {
    pub(crate) fn new(cancel: &'a CancellationToken, epoch: &'a dyn EpochPort,
        expected_epoch: u64, admission: &'a dyn AdmissionPort,
        work_limit: u64, byte_limit: u64, recursion_limit: u64) -> ProviderResult<Self> {
        if work_limit == 0 || byte_limit == 0 || recursion_limit == 0 { return Err(Error::Budget); }
        let context = Self { cancel, epoch, expected_epoch, admission, work_limit, byte_limit,
            recursion_limit, work: Cell::new(0), depth: Cell::new(0),
            glyph_limit: Cell::new(0), lookup_limit: Cell::new(0),source:Cell::new(None) };
        context.poll()?;
        Ok(context)
    }
    pub(crate) fn poll(&self) -> ProviderResult<()> {
        self.cancel.check().map_err(|_| Error::Canceled)?;
        if self.epoch.current_epoch()? != self.expected_epoch { return Err(Error::StaleEpoch); }
        Ok(())
    }
    pub(crate) fn step(&self) -> ProviderResult<()> { self.units(1) }
    pub(crate) fn units(&self, units: u64) -> ProviderResult<()> {
        self.poll()?;
        let next = self.work.get().checked_add(units).ok_or(Error::Overflow)?;
        if next > self.work_limit { return Err(Error::Budget); }
        self.work.set(next);
        Ok(())
    }
    pub(crate) fn work(&self) -> u64 { self.work.get() }
    pub(crate) fn source(&self)->Option<crate::SourceRange> {self.source.get()}
    pub(crate) fn set_source(&self,source:Option<crate::SourceRange>) {self.source.set(source);}
    pub(crate) fn set_shape_limits(&self, glyphs: u64, lookups: u64) -> ProviderResult<()> {
        self.poll()?;
        if glyphs == 0 || lookups == 0 { return Err(Error::Budget); }
        self.glyph_limit.set(glyphs);
        self.lookup_limit.set(lookups);
        Ok(())
    }
    pub(crate) fn check_glyphs(&self, count: u64) -> ProviderResult<()> {
        self.poll()?;
        if self.glyph_limit.get() == 0 || count > self.glyph_limit.get() { return Err(Error::Budget); }
        Ok(())
    }
    pub(crate) fn check_lookups(&self, count: u64) -> ProviderResult<()> {
        self.poll()?;
        if self.lookup_limit.get() == 0 || count > self.lookup_limit.get() { return Err(Error::Budget); }
        Ok(())
    }
    pub(crate) fn check_elements(&self, elements: u64) -> ProviderResult<()> {
        self.poll()?;
        if elements > self.work_limit { return Err(Error::Budget); }
        Ok(())
    }
    pub(crate) fn reserve(&self, bytes: u64) -> ProviderResult<Reservation<'a>> {
        self.step()?;
        if bytes == 0 || bytes > self.byte_limit { return Err(Error::Budget); }
        let before = self.admission.snapshot()?;
        let expected = before.current_requested_bytes.checked_add(bytes).ok_or(Error::Overflow)?;
        if expected > self.byte_limit { return Err(Error::Budget); }
        let lease = self.admission.reserve(bytes, self.byte_limit)?;
        if lease.requested_bytes() != bytes { return Err(Error::LeaseUnavailable); }
        let after = self.admission.snapshot()?;
        if after.current_requested_bytes != expected
            || after.operation_peak_requested_bytes < expected
            || after.operation_peak_requested_bytes > self.byte_limit {
            return Err(Error::LeaseUnavailable);
        }
        self.poll()?;
        Ok(lease)
    }
    pub(crate) fn enter(&self) -> ProviderResult<Depth<'_>> {
        self.step()?;
        let next = self.depth.get().checked_add(1).ok_or(Error::Overflow)?;
        if next > self.recursion_limit { return Err(Error::Budget); }
        self.depth.set(next);
        Ok(Depth { depth: &self.depth })
    }
}
pub(crate) struct Depth<'a> { depth: &'a Cell<u64> }
impl Drop for Depth<'_> {
    fn drop(&mut self) { self.depth.set(self.depth.get() - 1); }
}
