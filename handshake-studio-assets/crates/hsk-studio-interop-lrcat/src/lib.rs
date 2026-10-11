//! Internal bounded read-only Lightroom .lrcat file-byte decoder, fixed Lightroom semantic
//! extraction only; explicitly granted stable main/WAL handles. No engine, query API, writable
//! catalog or DB. Outputs CKC/Studio proposals through shared ports.
#![forbid(unsafe_code)]

mod catalog_bytes;
mod error;
mod extract;
mod source;
mod wal_snapshot;

pub use error::{LimitKind, LrcatError, Result, SourceError};
pub use extract::{
    CONVERSION_NOTICE, CatalogProposal, CollectionKind, CollectionMember, CollectionProposal,
    DECODER_ID, DevelopProposal, Disposition, EntityCount, KNOWN_PROCESS_VERSIONS,
    KNOWN_SCHEMA_VERSIONS, KeywordAssignment, KeywordProposal, LossNote, OpaqueBlob,
    PhotoProposal, WalReceipt, extract_catalog,
};
pub use source::{
    ByteSource, Grant, HARD_MAX_MAIN_BYTES, HARD_MAX_RETAINED_TEXT_BYTES, HARD_MAX_WAL_BYTES,
    Limits, SliceSource, WalGrant,
};
pub use wal_snapshot::Fingerprint;

use hsk_studio_accord::CancellationToken;
use hsk_studio_observe::{Observation, Observe, Outcome, Receipt, SinkPort};

/// The complete public decoding surface (everything else in this crate is `pub(crate)` or private).
pub const PUBLIC_ENTRYPOINTS: &[&str] = &["extract_catalog", "report"];

/// Maps an extraction result to exactly one Observe terminal outcome (no ids, paths or row text).
/// A proposal that carries losses is still `Success`; the loss list is the detail.
/// `UngrantedWal`, `SourceChanged` and source failures are `Failure(Unavailable)`; corrupt, cyclic,
/// over-limit and not-SQLite inputs `Failure(Validation)`; encrypted and unsupported container
/// features `Failure(Unsupported)`; cancellation `Canceled`. `correlation_id` and `revision` must
/// equal the values `observe` was created with.
pub fn report<T>(
    result: &Result<T>,
    correlation_id: u64,
    revision: u64,
    cancel: &CancellationToken,
    observe: &mut Observe,
    sink: &mut (impl SinkPort + ?Sized),
) -> std::result::Result<Receipt, hsk_studio_observe::Error> {
    let outcome = match result {
        Ok(_) => Outcome::Success,
        Err(error) => match error.failure_code() {
            None => Outcome::Canceled,
            Some(code) => Outcome::Failure(code),
        },
    };
    observe.emit(
        Observation {
            correlation_id,
            revision,
            outcome,
            progress: None,
            private_project_text: None,
        },
        cancel,
        sink,
    )
}

/// Same descriptor for models, human manual and Argus/diagnostic adapters.
pub const DESCRIPTOR: &str = r#"{"owner":"STUDIO-MODULE-INTEROP_LRCAT","version":1,"scope":"fixed extraction only: no SQL engine, no query API, no writable catalog, no repair, no private database or cache; the decoder reads untrusted file bytes and emits typed import proposals plus provenance and loss receipts","public_entrypoints":["extract_catalog(Grant, Limits, CancellationToken) -> CatalogProposal","report(result, correlation_id, revision, CancellationToken, Observe, SinkPort) -> Receipt"],"operator":{"intent":"Bring a Lightroom Classic catalog (.lrcat) into Studio as import proposals without trusting or modifying the file","result":"CatalogProposal: photos (path relative to root folder, capture time, rating, pick flag, color label, orientation), develop settings kept opaque and hashed, collections, keywords, per-entity counts where rows_seen == mapped + lost, and a loss list; nothing is committed","recovery":"ungranted_wal: grant the -wal sibling bytes or state that none exists; source_changed: re-grant stable immutable handles; encrypted_or_unrecognized / not_sqlite: the file is not a readable plain catalog; limit_*: raise the named limit or split the input; missing_required_table: the file is not a Lightroom Classic catalog; canceled: nothing was produced"},"model":{"grant":"Grant{main: ByteSource, wal: None|Granted(ByteSource)|PresentButNotGranted}; the caller states honestly whether a -wal sibling exists; PresentButNotGranted fails closed because the catalog could be stale","bytes":"SQLite file format header, table b-tree interior 0x05 and leaf 0x0d pages, varints, records, overflow pages, sqlite_master, committed-prefix WAL overlay; iterative walk with depth cap, visited-page cycle detection, overflow chain cap, page-read budget; limits are checked before the allocation or read they guard; an overflow page may be consumed by one chain only, so reuse by another row or table is refused as overflow_shared and a loop as overflow_cycle (test shared_overflow_chain_across_rows_is_refused); text kept in the schema, lookup tables and proposal is charged per copy against max_retained_text_bytes and exceeding it is limit_retained_text (test retained_text_budget_counts_every_copy); size fields at their numeric maximum are refused as typed errors (test sizes_at_numeric_maximum_are_typed_refusals)","accounting":"per entity: rows_seen is counted by the b-tree walker, mapped is the number of entries the proposal holds, lost is the number of rows that produced no entry (each a whole-row loss note, e.g. orphan_row or duplicate_rowid); field-level losses of a mapped row are listed separately; CatalogProposal::reconciles cross-checks the three against the proposal content and fails for a tampered proposal (test reconciliation_is_cross_checked_not_constructed)","tables_read":["AgLibraryRootFolder","AgLibraryFolder","AgLibraryFile","Adobe_images (required)","Adobe_imageDevelopSettings","AgLibraryCollection","AgLibraryCollectionImage","AgLibraryKeyword","AgLibraryKeywordImage","Adobe_variablesTable"],"tables_counted_as_loss":["AgLibraryCollectionContent","Adobe_AdditionalMetadata","AgHarvestedExifMetadata","Adobe_libraryImageDevelopHistoryStep","Adobe_libraryImageDevelopSnapshot"],"fingerprint":"main_len, wal_len, committed_wal_len, SHA-256 of main and WAL taken before and after decoding; a difference is source_changed","wal":"only frames up to the last valid commit frame (salt and checksum chain) are overlaid; an uncommitted or invalid tail is ignored and listed as a loss","dispositions":"editable, translated, approximate, opaque, unsupported, missing"},"limits":{"max_main_bytes":1073741824,"max_wal_bytes":268435456,"max_pages":2097152,"max_page_reads":16000000,"max_rows":4000000,"max_cells":16000000,"max_payload":16777216,"max_overflow_pages":4096,"max_btree_depth":64,"max_columns":2048,"max_unmapped_tables":256,"max_retained_opaque_bytes":268435456,"max_retained_text_bytes":536870912},"safety":"the library opens no path and writes nothing: the granted bytes hash identically before and after success and failure, and no SQL engine appears in the manifest or sources (test cancel_stale_no_source_write_no_sql_closure); decoded rows are proposals, not authority","cancel":"CancellationToken is checked per page read, per row and per hash chunk; a canceled call returns canceled and no partial proposal","undo":"proposals only; the host records an applied revision as one reversible step","argus":{"inspect":"CatalogProposal losses, counts and fingerprint","action":"extract_catalog with the same grant","capture":"caller captures the granted bytes hash, the proposal and the Observe frame"},"diagnostics":"report() emits one Observe terminal outcome; no ids, paths or row text are serialized by this crate","unverified":["Lightroom table and column names are taken from the module brief and a concept reference, not yet checked against a Lightroom-written catalog","Adobe_variablesTable name Adobe_DBVersion as the schema version carrier","AgLibraryCollection.creationId values for smart collections","community mapping of Camera Raw ProcessVersion internal numbers"],"notice":"parser success and row counts are not full conversion: every unmapped, opaque or unsupported item is listed in losses","next":"WAL-aware multi-snapshot receipts, XMP inflate with an output cap, develop settings semantics, CKC promotion through shared ports"}"#;
