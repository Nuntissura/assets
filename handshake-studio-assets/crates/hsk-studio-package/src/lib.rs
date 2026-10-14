//! Native document container serialization, manifest/hash references, atomic recovery and bounded
//! loading. Stream asset bytes through CKC/PRIM-ArtifactService provider ports; no private catalog,
//! preview cache, metadata store, watcher or second CAS under STU-ASSET-004/009/010. Standalone
//! fixtures supply scoped in-memory resolver inputs rather than kernel imports.
#![forbid(unsafe_code)]

mod bounded_zip;
mod error;
mod hashing;
mod limits;
mod manifest;
mod read;
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
pub use limits::Limits;
pub use manifest::{
    ASSET_PREFIX, CONTAINER_VERSION, Codec, DOCUMENT_NAME, EntryKind, MANIFEST_NAME, Manifest,
    ManifestEntry, PREVIEW_PREFIX, PixelFormat, PreviewInfo,
};
pub use read::{
    LossNote, LossReason, Opened, PackageReader, PreviewImage, PreviewRef, Quarantined,
    list_previews, read_package, read_package_stream, read_preview,
};
pub use report::{outcome_for, outcome_for_opened, report};
pub use save::{
    AtomicFs, StdFs, StreamSave, StreamingFs, SweepReport, save_atomic, save_atomic_streaming,
    save_atomic_streaming_with, save_atomic_with, sweep_stale_temps,
};
pub use shuffle::{byte_shuffle, byte_unshuffle};
pub use stream::write_package_streaming;
pub use write::{
    AssetReader, AssetSource, AssetStream, Extras, MemoryAssetSource, WriteOptions, WriteReceipt,
    write_package, write_package_with,
};
pub use zip::{EntryMeta, Zip64Policy, preflight, preflight_stream};

pub const DESCRIPTOR: &str = r#"{"owner":"STUDIO-MODULE-PACKAGE","version":1,"api":"write_package / write_package_with(Extras, WriteOptions) / write_package_streaming(AssetStream, Write+Seek) / read_package / read_package_stream / PackageReader / preflight / preflight_stream / list_previews / read_preview / byte_shuffle / byte_unshuffle / save_atomic / save_atomic_with / save_atomic_streaming(StreamSave) / save_atomic_streaming_with(StreamingFs) / sweep_stale_temps / outcome_for / outcome_for_opened / report","consumer":"package-consumer --write DOC.json OUT.handshake [--assets DIR] [--zstd|--shuffle] [--stream] [--preview PIXELS FORMAT WxH COLOR] | --read IN.handshake | --sweep TARGET | --descriptor","input":"Folio document bytes plus digest-keyed asset bytes granted through an AssetSource (whole buffers) or an AssetStream (bounded readers with declared length, SHA-256 verified while streaming), optional preview images and preserved opaque entries; or a .handshake container as a byte slice or a Read+Seek stream","output":"Deterministic STORE-only ZIP/ZIP64 container: manifest.json first, document.json verbatim, assets/<sha256hex> (raw, zstd or shuffle_zstd with elem_size), previews/<sha256hex> (declared rgba8|rgb8|gray8, width, height, colour tag), preserved opaque entries. On read: Opened with verified decoded assets, quarantined entries, loss notes and declared previews (bodies never read by read_package)","limits":{"max_entries":65536,"max_entry_bytes":536870912,"max_total_bytes":4294967296,"max_decoded_total_bytes":4294967296,"max_name_len":255,"max_manifest_bytes":16777216,"max_window_bytes":67108864,"max_preview_bytes":16777216,"max_preview_dimension":16384},"container":"STORE-only ZIP/ZIP64 with canonical-form reader (single extra field kind: ZIP64; ZIP64 only when an EOCD sentinel announces it); blob codecs raw|zstd|shuffle_zstd chosen per blob as the smallest; ascii [A-Za-z0-9._-] names","recovery":"Hostile or invalid input is rejected with a stable code and no partial state; unknown container_version is UnsupportedVersion without decode; unknown entries are quarantined verbatim with a loss note; decoded sizes are bounded per entry, in aggregate and by zstd window before any decode; atomic save keeps the last-good destination byte-identical on any failure and removes only its own temp file; sweep_stale_temps removes only temps named by a valid unlocked owner marker of a dead process","manual":"Write: build a folio Snapshot, grant asset bytes via MemoryAssetSource or a host port, call write_package/write_package_with into a Vec, then save_atomic. Streaming write: grant an AssetStream (MemoryAssetSource implements it; a host returns one bounded reader per digest, same bytes on every open) and call save_atomic_streaming(target, &StreamSave{doc,assets,extras,options}, limits, token) or write_package_streaming into any Write+Seek positioned at 0; the output is byte-identical to write_package_with. Read: read_package (slice) or read_package_stream/PackageReader (Read+Seek) with Limits, folio Budget and a folio Resolver; inspect Opened.loss and Opened.previews; fetch pixels with read_preview or PackageReader::read_preview. After a crash call sweep_stale_temps(target).","diagnostics":"PackageError::code() stable snake_case codes; report() maps outcome classes onto Observe (Success, Canceled, Failure Validation/Unavailable/Loss); no names, paths or document text are emitted","authority":"Transport only: not a grant, live database, catalog or second CAS; serialized grants confer no authority. Streaming save memory: peak heap does not grow with asset size or count (raw assets pass through one 1 MiB chunk; a zstd or shuffle_zstd asset is read twice, once to plan its stored length for the manifest and once to write it, buffering one blob at a time at roughly 3-4x the blob plus encoder state); the finished temp file is re-verified from disk (CRCs, manifest, every SHA-256) before the atomic replace. Pending: ArtifactService/CKC wiring, sanitized delivery (STU-IO-188), import identity remap (STU-IO-190), kill during the rename call itself, durability across power loss (std offers no directory flush on Windows, so rename is atomic but not proven durable there; a unix parent-directory fsync failure is ignored), lock-based stale sweep on network filesystems, shuffle only for tiles whose format name carries a 2/4/8-byte element. Previews are derived data: re-saving an opened document drops them unless supplied again"}"#;
