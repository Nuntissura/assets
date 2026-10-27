//! Pure source-library and appearance reducers. The host owns authorization and durable CAS commits.
pub mod sources;
mod groups;
mod editing;
mod source_tags;
mod rules;
pub mod visibility;
pub mod appearance_state;
pub use sources::{SourceGroup, SourceLibrary};
pub use rules::{MemberRule, RuleLimits};
pub use visibility::ProfileSelection;
pub use appearance_state::{group_items, update_state, ItemAppearances};
pub mod query;
