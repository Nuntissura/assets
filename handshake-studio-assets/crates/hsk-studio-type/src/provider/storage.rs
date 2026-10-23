use super::{Context, ProviderResult};
use crate::{Error, Reservation};
use std::{alloc::{alloc, Layout}, ops::{Deref, DerefMut}};

/// Exact-capacity private storage. Fields drop payload before reservation.
pub(crate) struct ChargedVec<'a, T> {
    values: Vec<T>,
    lease: Option<Reservation<'a>>,
}
impl<'a, T> ChargedVec<'a, T> {
    pub(crate) fn new() -> Self { Self { values: Vec::new(), lease: None } }
    pub(crate) fn with_capacity(cap: usize, ctx: &Context<'a>) -> ProviderResult<Self> {
        let mut result = Self::new(); result.reserve_total(cap, ctx)?; Ok(result)
    }
    pub(crate) fn capacity(&self) -> usize { self.values.capacity() }
    pub(crate) fn len(&self) -> usize { self.values.len() }
    pub(crate) fn is_empty(&self) -> bool { self.values.is_empty() }
    pub(crate) fn as_slice(&self) -> &[T] { &self.values }
    pub(crate) fn as_mut_slice(&mut self) -> &mut [T] { &mut self.values }
    pub(crate) fn reserve_total(&mut self, cap: usize, ctx: &Context<'a>) -> ProviderResult<()> {
        ctx.step()?;
        ctx.check_elements(u64::try_from(cap).map_err(|_| Error::Overflow)?)?;
        if cap <= self.values.capacity() { return Ok(()); }
        let layout = Layout::array::<T>(cap).map_err(|_| Error::Overflow)?;
        if layout.size() == 0 { return Ok(()); }
        ctx.units(u64::try_from(self.values.len()).map_err(|_| Error::Overflow)?)?;
        let lease = ctx.reserve(u64::try_from(layout.size()).map_err(|_| Error::Overflow)?)?;
        ctx.poll()?;
        // SAFETY: checked nonzero exact layout; null preserves old storage.
        let pointer = unsafe { alloc(layout) }.cast::<T>();
        if pointer.is_null() { return Err(Error::Allocation); }
        // SAFETY: exact capacity allocation, zero initialized length.
        let mut replacement = unsafe { Vec::from_raw_parts(pointer, 0, cap) };
        if let Err(error) = ctx.poll() { drop(replacement); drop(lease); return Err(error); }
        let len = self.values.len();
        // SAFETY: disjoint blocks, sufficient capacity, ownership moved without callbacks.
        unsafe {
            std::ptr::copy_nonoverlapping(self.values.as_ptr(), replacement.as_mut_ptr(), len);
            replacement.set_len(len); self.values.set_len(0);
        }
        let old = std::mem::replace(&mut self.values, replacement);
        drop(old);
        self.lease = Some(lease);
        Ok(())
    }
    pub(crate) fn push(&mut self, value: T, ctx: &Context<'a>) -> ProviderResult<()> {
        ctx.step()?;
        let len = self.len().checked_add(1).ok_or(Error::Overflow)?;
        self.reserve_total(len, ctx)?;
        self.values.push(value); Ok(())
    }
    pub(crate) fn extend_copy(&mut self, values: &[T], ctx: &Context<'a>) -> ProviderResult<()> where T: Copy {
        let len = self.len().checked_add(values.len()).ok_or(Error::Overflow)?;
        self.reserve_total(len, ctx)?;
        for value in values { self.push(*value, ctx)?; }
        Ok(())
    }
    pub(crate) fn resize_copy(&mut self, len: usize, value: T, ctx: &Context<'a>) -> ProviderResult<()> where T: Copy {
        self.reserve_total(len, ctx)?;
        while self.len() < len { self.push(value, ctx)?; }
        self.truncate(len); Ok(())
    }
    pub(crate) fn insert(&mut self, index: usize, value: T, ctx: &Context<'a>) -> ProviderResult<()> {
        if index > self.len() { return Err(Error::InvalidInput); }
        ctx.units(u64::try_from(self.len() - index).map_err(|_| Error::Overflow)?)?;
        let len = self.len().checked_add(1).ok_or(Error::Overflow)?;
        self.reserve_total(len, ctx)?; self.values.insert(index, value); Ok(())
    }
    pub(crate) fn remove(&mut self, index: usize, ctx: &Context<'a>) -> ProviderResult<T> {
        if index >= self.len() { return Err(Error::InvalidInput); }
        ctx.units(u64::try_from(self.len() - index).map_err(|_| Error::Overflow)?)?;
        Ok(self.values.remove(index))
    }
    pub(crate) fn truncate(&mut self, len: usize) { self.values.truncate(len); }
    pub(crate) fn clear(&mut self) { self.values.clear(); }
    pub(crate) fn pop(&mut self) -> Option<T> { self.values.pop() }
    pub(crate) fn retype<U: bytemuck::Pod>(self) -> ProviderResult<ChargedVec<'a, U>> where T: bytemuck::Pod {
        if std::mem::size_of::<T>() != std::mem::size_of::<U>() || std::mem::align_of::<T>() != std::mem::align_of::<U>() {
            return Err(Error::InternalInvariant);
        }
        let mut this = std::mem::ManuallyDrop::new(self);
        let lease = this.lease.take();
        // SAFETY: equal layout, all bit patterns valid; sole block/lease ownership transfers.
        let values = unsafe { Vec::from_raw_parts(this.values.as_mut_ptr().cast::<U>(), this.values.len(), this.values.capacity()) };
        Ok(ChargedVec { values, lease })
    }
}
impl<T> Default for ChargedVec<'_, T> { fn default() -> Self { Self::new() } }
impl<T> Deref for ChargedVec<'_, T> { type Target = [T]; fn deref(&self) -> &[T] { &self.values } }
impl<T> DerefMut for ChargedVec<'_, T> { fn deref_mut(&mut self) -> &mut [T] { &mut self.values } }
