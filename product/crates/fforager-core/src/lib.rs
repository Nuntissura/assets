//! Deterministic planning, resource ownership, lifecycle models and job orchestration.
//! Concrete network and filesystem effects remain behind inward-facing ports.

#![forbid(unsafe_code)]

pub mod direct;
pub mod engine;
pub mod lifecycle;
pub mod resource;

mod admission;
