//! Nib: fallible, source-only geometry proposals; no document mutation or host authority.
mod encoded;
mod owned_path;
mod path_contract;
mod provider;
pub use owned_path::*;
pub use path_contract::*;
pub use provider::{Network, Region, Segment};
mod proposal;
pub use proposal::*;

mod descriptor;
pub mod anchor_path;
pub mod boolean;
pub mod edit;
pub mod flatten;
pub mod freeform;
pub mod network;
pub mod pathfinder;
pub mod raster;
pub mod shape;
pub mod stroke;
pub use descriptor::descriptor;
