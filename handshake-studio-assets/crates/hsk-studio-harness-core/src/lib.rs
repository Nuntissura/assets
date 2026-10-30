//! Core half of STUDIO-MODULE-HARNESS (SMB-CRATE-SEAM-SPLITS slice 3b): the headless run over the
//! real session (open, revision-checked provider submit, atomic save, reopen), the import routes
//! (features import, import-psd, import-raw, import-tiff, import-pdf, import-raster), the
//! controls-descriptor panel and the manual. `hsk-studio-harness` is the public face; the
//! `#[doc(hidden)]` module is the seam `hsk-studio-harness-view` and the CLI build on.
#![forbid(unsafe_code)]

pub mod action;
#[doc(hidden)]
pub mod harness;
pub mod import;
pub mod manual;
pub mod panel;

pub use action::{ActionRecord, ActionSet, ActionStep, PlayMode, RecordedValue, RecordingReport};
pub use harness::{
    AVAILABLE_PROVIDERS, Harness, HarnessError, NOT_PROVEN, Place, PlacementWrite, References, Run, SCHEMA, Selection, default_actor,
};
