//! Portable canonical workspace ownership; see the product manual for scope and recovery.
#![forbid(unsafe_code)]

mod arrangement;
mod command;
mod core;
#[cfg(feature = "serde")]
mod decimal;
mod docking;
mod geometry;
mod identity;
mod ids;
mod layout;
mod layout_actions;
mod model;
mod mutation;
mod persistence;
mod restore;
mod solver;
mod tabs;
mod tools;

pub use command::*;
pub use core::Core;
pub use docking::*;
pub use geometry::*;
pub use identity::*;
pub use ids::*;
pub use layout::*;
pub use model::*;
pub use persistence::*;
pub use restore::*;
pub use tabs::*;
pub use tools::*;
