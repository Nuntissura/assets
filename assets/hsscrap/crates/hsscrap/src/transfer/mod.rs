//! Portable projections and import plans; hosts authorize, sign, persist and commit them.
//!
//! The host supplies a complete authorized snapshot, a durable exporter cursor and destination
//! mappings. Plans never mutate source records, grant access or execute imported owner commands.
//! Source hashes are checked before reference rewriting; local gallery digests are computed afterward.
//!
//! Basis: parent `transfer_projection`, RFC8785 sections 3.1/3.2 and Appendix B, RFC7493 section 2.2,
//! the published handshake_canonical_json strict APIs and existing typed hsscrap records/owner plans.
//! Reuse typed records, decimal revisions and the owning JCS implementation. Reject foreign UUID
//! equality as a mapping, legacy canonical bytes for arbitrary payloads and copied media receipts.
//! Exact integers outside the interoperable binary64 range must be strings. Aggregate payload limits
//! precede typed serialization. Host envelopes remain responsible for trust, audience and live grants.

mod import;
mod model;
mod projection;
mod references;
pub use import::*;
pub use model::*;
pub use projection::*;
#[cfg(test)]
mod tests;
