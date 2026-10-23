//! Requested layouts/capacities, not physical heap/RSS. Pinned Rust Arc layout provenance
//! is recorded in SOURCE_PROVENANCE.json; independent allocation reconciliation remains required.
use crate::Error;
use std::{alloc::Layout, sync::atomic::AtomicUsize};
#[repr(C, align(2))]
struct ArcHeader {
    strong: AtomicUsize,
    weak: AtomicUsize,
    data: (),
}
pub fn array<T>(count: usize) -> Result<u64, Error> {
    let layout = Layout::array::<T>(count).map_err(|_| Error::Overflow)?;
    u64::try_from(layout.size()).map_err(|_| Error::Overflow)
}
pub fn arc(layout: Layout) -> Result<u64, Error> {
    let layout = Layout::new::<ArcHeader>()
        .extend(layout)
        .map_err(|_| Error::Overflow)?
        .0
        .pad_to_align();
    u64::try_from(layout.size()).map_err(|_| Error::Overflow)
}
pub fn arc_bytes(count: usize) -> Result<u64, Error> {
    arc(Layout::array::<u8>(count).map_err(|_| Error::Overflow)?)
}
pub fn add(total: &mut u64, n: u64) -> Result<(), Error> {
    *total = total.checked_add(n).ok_or(Error::Overflow)?;
    Ok(())
}
