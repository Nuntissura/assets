//! Asset half of STUDIO-MODULE-WEB (SMB-CRATE-SEAM-SPLITS-2): the asset reference gate
//! (`collect_assets`: carrier inventory and load contexts, CSS Syntax 3 URL tokenizer, data: URL
//! reading), on the core half's DOM, character references and errors.
//! `hsk-studio-web` is the public face.
#![forbid(unsafe_code)]

// The core half's modules under the names these modules have always used (`crate::dom`, ...).
use hsk_studio_web_core::{charref, dom, error};
// Root names the moved unit tests use (`crate::parse`, `crate::Limits`, `crate::DESCRIPTOR`).
#[cfg(test)]
use hsk_studio_web_core::{DESCRIPTOR, Limits, parse};

mod assets;
mod carriers;
mod cssurl;
mod datauri;

pub use assets::{
    AssetDecision, AssetOutcome, AssetPort, AssetRef, AssetSource, DenyReason, MAX_DATA_NESTING,
    collect_assets,
};
pub use carriers::{Load, Loads};
