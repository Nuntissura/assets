use crate::sources::SourceLibrary;
use rss_contracts::{Bounds, Error, HostRef, Result, Revision, ScopeRef};

impl SourceLibrary {
    /// Updates source presentation only. Changing provider identity or access mode requires a new follow intent.
    pub fn edit_source(&self, scope: &ScopeRef, expected: Revision, source_ref: &HostRef,
        expected_source: Revision, display_name: &str, bounds: &Bounds) -> Result<Self> {
        self.authorize(scope, expected)?;
        bounds.validate()?;
        let current = self.sources.get(source_ref).ok_or(Error::NotFound)?;
        if &current.owner_scope != scope { return Err(Error::Denied); }
        if current.revision != expected_source { return Err(Error::Conflict); }
        if display_name.trim().is_empty() { return Err(Error::Invalid { field: "source.display_name".into() }); }
        bounds.text("source.display_name", display_name, bounds.max_text_bytes)?;
        if current.display_name == display_name { return Ok(self.clone()); }
        let mut next = self.clone();
        let changed = next.sources.get_mut(source_ref).ok_or(Error::NotFound)?;
        changed.display_name = display_name.to_owned();
        changed.revision = changed.revision.next()?;
        next.advance()?;
        Ok(next)
    }
}
