//! Invocation-local bridge for borrowed parsers whose normal result is optional.
use crate::Error;
use crate::provider::{Context, ProviderResult};
use std::cell::Cell;
pub(crate) struct Scope<'a, 'b> {
    context: &'a Context<'b>,
    failure: Cell<Option<Error>>,
}
impl Scope<'_, '_> {
    pub(crate) fn check(&self) -> Option<()> {
        if self.failure.get().is_some() {
            None
        } else {
            Some(())
        }
    }
    pub(crate) fn step(&self) -> Option<()> {
        if self.failure.get().is_some() {
            return None;
        }
        match self.context.step() {
            Ok(()) => Some(()),
            Err(error) => self.fail(error),
        }
    }
    pub(crate) fn fail<T>(&self, error: Error) -> Option<T> {
        if self.failure.get().is_none() {
            self.failure.set(Some(error));
        }
        None
    }
    pub(crate) fn take<T>(&self, result: ProviderResult<Option<T>>) -> Option<T> {
        match result {
            Ok(value) => value,
            Err(error) => self.fail(error),
        }
    }
}
pub(crate) fn run<T>(
    context: &Context<'_>,
    operation: impl FnOnce(&Scope<'_, '_>) -> Option<T>,
) -> ProviderResult<Option<T>> {
    context.step()?;
    let scope = Scope {
        context,
        failure: Cell::new(None),
    };
    let value = operation(&scope);
    if let Some(error) = scope.failure.get() {
        Err(error)
    } else {
        Ok(value)
    }
}
