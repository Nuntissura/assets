//! Nib: fallible, source-only geometry proposals; no document mutation or host authority.
mod encoded;
mod path_contract;
mod provider;
pub use path_contract::*;
pub use provider::{Network, Region, Segment};
mod proposal;
pub use proposal::*;

mod descriptor;
pub use descriptor::descriptor;
