//! Native document container serialization, manifest/hash references, atomic recovery and bounded
//! loading. Stream asset bytes through CKC/PRIM-ArtifactService provider ports; no private catalog,
//! preview cache, metadata store, watcher or second CAS under STU-ASSET-004/009/010. Standalone
//! fixtures supply scoped in-memory resolver inputs rather than kernel imports.
#![forbid(unsafe_code)]

mod bounded_zip;
mod error;
mod externals;
mod hashing;
mod import;
mod limits;
mod manifest;
mod read;
mod references;
mod report;
mod save;
mod shuffle;
mod stream;
mod write;
mod zip;

pub use error::{AssetError, PackageError};
pub use bounded_zip::{
    InflateFail, MAX_ZIP_NAME_LEN, ZipDirectory, ZipEntry, ZipLimits, ZipReadError, inflate_exact,
    read_zip,
};
pub use hashing::{crc32_of, sha256_hex_of};
pub use import::{IMPORT_PREFIX, ImportEntry, ImportRef, ImportRole};
pub use limits::Limits;
pub use manifest::{
    ASSET_PREFIX, CONTAINER_VERSION, Codec, DOCUMENT_NAME, EntryKind, MANIFEST_NAME, Manifest,
    ManifestEntry, PREVIEW_PREFIX, PixelFormat, PreviewInfo,
};
pub use read::{
    LossNote, LossReason, Opened, PackageReader, PreviewImage, PreviewRef, Quarantined,
    list_previews, read_package, read_package_stream, read_preview,
};
pub use externals::{ExternalKind, ExternalRef, LinkState, embeddable_digests, external_references};
pub use references::{
    AssetUse, DOCUMENT_ENTRY_PREFIX, PACKAGE_ASSET_SOURCE_KIND, ReferencedAsset, TILE_SCHEMA_ID, referenced_assets,
};
pub use report::{outcome_for, outcome_for_opened, report};
pub use save::{
    AtomicFs, StdFs, StreamSave, StreamingFs, SweepReport, save_atomic, save_atomic_streaming,
    save_atomic_streaming_with, save_atomic_with, sweep_stale_temps,
};
pub use shuffle::{byte_shuffle, byte_unshuffle};
pub use stream::write_package_streaming;
pub use write::{
    AssetReader, AssetSource, AssetStream, EmbedExternal, Extras, MemoryAssetSource, WriteOptions, WriteReceipt,
    write_package, write_package_with,
};
pub use zip::{EntryMeta, Zip64Policy, preflight, preflight_stream};

pub const DESCRIPTOR: &str = r#"{"owner":"STUDIO-MODULE-PACKAGE","version":1,"api":"write_package / write_package_with(Extras, WriteOptions) / write_package_streaming(AssetStream, Write+Seek) / read_package / read_package_stream / PackageReader / preflight / preflight_stream / list_previews / read_preview / referenced_assets / external_references / embeddable_digests / byte_shuffle / byte_unshuffle / save_atomic / save_atomic_with / save_atomic_streaming(StreamSave) / save_atomic_streaming_with(StreamingFs) / sweep_stale_temps / outcome_for / outcome_for_opened / report","consumer":"package-consumer --write DOC.json OUT.handshake [--assets DIR] [--zstd|--shuffle] [--stream] [--preview PIXELS FORMAT WxH COLOR] | --read IN.handshake | --sweep TARGET | --descriptor","input":"Folio document bytes plus digest-keyed asset bytes granted through an AssetSource (whole buffers) or an AssetStream (bounded readers with declared length, SHA-256 verified while streaming), optional preview images and preserved opaque entries; or a .handshake container as a byte slice or a Read+Seek stream","output":"Deterministic STORE-only ZIP/ZIP64 container: manifest.json first, document.json verbatim, assets/<sha256hex> (raw, zstd or shuffle_zstd with elem_size), previews/<sha256hex> (declared rgba8|rgb8|gray8, width, height, colour tag), preserved opaque entries, typed import entries import/<records|receipt|original>.<sha256hex> (opaque kind, reserved names: grammar, name digest and records/receipt <= max_manifest_bytes enforced on write and before decode on read; tests import_entries). On read: Opened with verified decoded assets, quarantined entries, loss notes, declared previews (bodies never read by read_package) and Opened::imports (typed refs, no loss note; bytes stay in quarantined so re-saves re-emit them; Opened::import_bytes / Opened::import)","limits":{"max_entries":65536,"max_entry_bytes":536870912,"max_total_bytes":4294967296,"max_decoded_total_bytes":4294967296,"max_name_len":255,"max_manifest_bytes":16777216,"max_window_bytes":67108864,"max_preview_bytes":16777216,"max_preview_dimension":16384},"container":"STORE-only ZIP/ZIP64 with canonical-form reader (single extra field kind: ZIP64; ZIP64 only when an EOCD sentinel announces it); blob codecs raw|zstd|shuffle_zstd chosen per blob as the smallest; ascii [A-Za-z0-9._-] names","assets":"The writers write, and the reader requires, every asset the document and its import/records.* JSON entries reference (referenced_assets, module references.rs, lane SMB-PACKAGE-MASK-TILES): every tile reference found by its required schema_id hsk.studio.raster_tile@1 (raster payload tiles, layer appearance mask coverage, smart-filter entry and stack masks, any future tile plane) and every records image source with source_kind package_asset; a referenced asset the AssetSource cannot supply fails the save typed; a malformed tile- or image-shaped records object is invalid_digest; non-JSON records are opaque. On read a missing raster payload tile is missing_asset, any other missing plane (a package written before the fix dropped mask assets) opens with LossReason::MissingAsset named assets/<sha256> (tests mask_tiles.rs, incl. every_folio_digest_position_is_classified over Folio's schemas)","externals":"Every reference from the document or a record entry (import/records.*, studio/*) to data outside the document JSON is kept as its link on every save and reported on read in Opened::externals with a closed link state (STU-ASSET-007: embedded, resolved, modified, missing, unauthorized, unsupported, unresolved) and relink data (handle, content hash, JSON-pointer location, occurrences): timeline media (Resolver::media), stored story text (Resolver::story_content), import-original parts (the import/original.<sha256> entry), primitives (Resolver::primitive), colour profiles incl. working/blending spaces (Resolver::profile), LUT references (cube carried by value = embedded, else unresolved), fonts and composition audio (no resolver port: unresolved), placed sources (embedded mode: the package asset of its sha256 or missing; linked mode: relative uri + digest, state from the recorded health) and ICC profiles (embedded asset, else Resolver::profile). missing/unauthorized/modified/unsupported add LossReason::UnavailableExternal named external/<kind>/<sha256 or handle> (module externals.rs, lane SMB-PACKAGE-COMPLETENESS). Bytes a record says travel with the document (embedded placed sources, ICC profiles) are embedded whenever the AssetSource grants them. WriteOptions::embed_external: Link (default, links only, STU-ASSET-008), Granted (embed timeline media and stored text the AssetSource grants as assets/<sha256>, keep the rest linked; session saves use it so an embedded copy stays embedded), All (self-contained: every one must be granted, else asset_absent with nothing written, STU-ASSET-009). Tests externals.rs, incl. every_folio_reference_property_is_classified over every committed Folio schema","recovery":"Hostile or invalid input is rejected with a stable code and no partial state; unknown container_version is UnsupportedVersion without decode; unknown entries are quarantined verbatim with a loss note; decoded sizes are bounded per entry, in aggregate and by zstd window before any decode; atomic save keeps the last-good destination byte-identical on any failure and removes only its own temp file; sweep_stale_temps removes only temps named by a valid unlocked owner marker of a dead process","manual":"Write: build a folio Snapshot, grant asset bytes via MemoryAssetSource or a host port, call write_package/write_package_with into a Vec, then save_atomic. Streaming write: grant an AssetStream (MemoryAssetSource implements it; a host returns one bounded reader per digest, same bytes on every open) and call save_atomic_streaming(target, &StreamSave{doc,assets,extras,options}, limits, token) or write_package_streaming into any Write+Seek positioned at 0; the output is byte-identical to write_package_with. Read: read_package (slice) or read_package_stream/PackageReader (Read+Seek) with Limits, folio Budget and a folio Resolver; inspect Opened.loss and Opened.previews; fetch pixels with read_preview or PackageReader::read_preview. After a crash call sweep_stale_temps(target).","diagnostics":"PackageError::code() stable snake_case codes; report() maps outcome classes onto Observe (Success, Canceled, Failure Validation/Unavailable/Loss); no names, paths or document text are emitted","authority":"Transport only: not a grant, live database, catalog or second CAS; serialized grants confer no authority. Streaming save memory: peak heap does not grow with asset size or count (raw assets pass through one 1 MiB chunk; a zstd or shuffle_zstd asset is read twice, once to plan its stored length for the manifest and once to write it, buffering one blob at a time at roughly 3-4x the blob plus encoder state); the finished temp file is re-verified from disk (CRCs, manifest, every SHA-256) before the atomic replace. Pending: ArtifactService/CKC wiring, sanitized delivery (STU-IO-188), import identity remap (STU-IO-190), kill during the rename call itself, durability across power loss (std offers no directory flush on Windows, so rename is atomic but not proven durable there; a unix parent-directory fsync failure is ignored), lock-based stale sweep on network filesystems, shuffle only for tiles whose format name carries a 2/4/8-byte element. Previews are derived data: re-saving an opened document drops them unless supplied again"}"#;
