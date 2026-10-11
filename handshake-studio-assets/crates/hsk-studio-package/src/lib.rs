//! Native document container serialization, manifest/hash references, atomic recovery and bounded
//! loading. Stream asset bytes through CKC/PRIM-ArtifactService provider ports; no private catalog,
//! preview cache, metadata store, watcher or second CAS under STU-ASSET-004/009/010. Standalone
//! fixtures supply scoped in-memory resolver inputs rather than kernel imports.
#![forbid(unsafe_code)]

mod error;
mod hashing;
mod limits;
mod manifest;
mod read;
mod report;
mod save;
mod write;
mod zip;

pub use error::{AssetError, PackageError};
pub use hashing::sha256_hex_of;
pub use limits::Limits;
pub use manifest::{
    ASSET_PREFIX, CONTAINER_VERSION, DOCUMENT_NAME, EntryKind, MANIFEST_NAME, Manifest,
    ManifestEntry, PREVIEW_PREFIX,
};
pub use read::{LossNote, LossReason, Opened, Quarantined, read_package};
pub use report::{outcome_for, outcome_for_opened, report};
pub use save::{AtomicFs, StdFs, save_atomic, save_atomic_with};
pub use write::{
    AssetSource, MemoryAssetSource, WriteOptions, WriteReceipt, write_package, write_package_with,
};
pub use zip::{EntryMeta, Zip64Policy, preflight};

pub const DESCRIPTOR: &str = r#"{"owner":"STUDIO-MODULE-PACKAGE","version":1,"api":"write_package / write_package_with / read_package / preflight / save_atomic / outcome_for / report","consumer":"package-consumer --write DOC.json OUT.handshake [--assets DIR] | --read IN.handshake | --descriptor","input":"Folio document bytes plus digest-keyed asset bytes granted through an AssetSource; or a .handshake container byte slice","output":"Deterministic STORE-only ZIP/ZIP64 container (manifest.json first, document.json verbatim, assets/<sha256hex>, preserved opaque entries); on read an Opened value with verified assets, quarantined entries and loss notes","limits":{"max_entries":65536,"max_entry_bytes":536870912,"max_total_bytes":4294967296,"max_name_len":255,"max_manifest_bytes":16777216,"compression":"store_only","names":"ascii [A-Za-z0-9._-] components joined by slash"},"recovery":"Hostile or invalid input is rejected with a stable code and no partial state; unknown container_version is UnsupportedVersion without decode; unknown entries are quarantined verbatim with a loss note; atomic save keeps the last-good destination byte-identical on any failure and removes only its own temp file","manual":"Write: build a folio Snapshot, grant asset bytes via MemoryAssetSource or a host port, call write_package into a Vec, then save_atomic. Read: read_package with Limits, folio Budget and a folio Resolver; inspect Opened.loss.","diagnostics":"PackageError::code() stable snake_case codes; report() maps outcome classes onto Observe (Success, Canceled, Failure Validation/Unavailable/Loss); no names, paths or document text are emitted","authority":"Transport only: not a grant, live database, catalog or second CAS; serialized grants confer no authority. ArtifactService/CKC wiring, previews, sanitized delivery (STU-IO-188), import identity remap (STU-IO-190), streaming from Read and kill-during-rename proof remain pending"}"#;
