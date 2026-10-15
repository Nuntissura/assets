//! Internal bounded read-only Lightroom .lrcat file-byte decoder, fixed Lightroom semantic
//! extraction only; explicitly granted stable main/WAL handles. No engine, query API, writable
//! catalog or DB. Outputs CKC/Studio proposals through shared ports.
#![forbid(unsafe_code)]

mod catalog_bytes;
mod ckc_import;
mod develop_map;
mod error;
mod extract;
mod library;
mod lua_rules;
mod source;
mod wal_snapshot;
mod xmp;

pub use ckc_import::{CkcImportPlan, PlannedMetadata, apply_ckc_import, plan_ckc_import};
pub use develop_map::{DevelopMapping, MAPPED_DEVELOP_KEYS, WhiteBalanceSource};
pub use error::{LimitKind, LrcatError, Result, SourceError};
pub use extract::{
    CONVERSION_NOTICE, CatalogProposal, CollectionKind, CollectionMember, CollectionProposal,
    DECODER_ID, DevelopProposal, Disposition, EntityCount, KNOWN_PROCESS_VERSIONS,
    KNOWN_SCHEMA_VERSIONS, KeywordAssignment, KeywordProposal, LossNote, OpaqueBlob,
    PhotoProposal, WalReceipt, extract_catalog,
};
pub use library::{
    ColorLabel, Flag, HARD_MAX_HIERARCHY_DEPTH, HierarchyNode, HierarchyStatus, LIBRARY_DECODER_ID,
    LibraryCollection, LibraryCollectionKind, LibraryKeyword, LibraryLimits, LibraryProposal,
    PhotoLibraryState, extract_library, resolve_hierarchy,
};
pub use lua_rules::{
    Combine, HARD_MAX_RULE_DEPTH, HARD_MAX_RULE_INPUT_BYTES, HARD_MAX_RULE_NODES,
    HARD_MAX_RULE_STRING_BYTES, KnownField, KnownOp, Named, RuleCriterion, RuleError, RuleGroup,
    RuleLimits, RuleNode, RuleScalar, SmartRules, parse_smart_rules,
};
pub use source::{
    ByteSource, Grant, HARD_MAX_MAIN_BYTES, HARD_MAX_RETAINED_TEXT_BYTES, HARD_MAX_WAL_BYTES,
    Limits, SliceSource, WalGrant,
};
pub use wal_snapshot::Fingerprint;
pub use xmp::{
    HARD_MAX_XMP_DEPTH, HARD_MAX_XMP_INPUT_BYTES, HARD_MAX_XMP_KEYWORDS, HARD_MAX_XMP_NODES,
    HARD_MAX_XMP_TEXT_BYTES, NS_DC, NS_LR, NS_RDF, NS_XMP, XmpError, XmpLibraryFields, XmpLimits,
    XmpRating, decode_xmp_packet,
};

use hsk_studio_accord::CancellationToken;
use hsk_studio_observe::{Observation, Observe, Outcome, Receipt, SinkPort};

/// The complete public decoding surface (everything else in this crate is `pub(crate)` or private).
pub const PUBLIC_ENTRYPOINTS: &[&str] = &[
    "extract_catalog",
    "extract_library",
    "parse_smart_rules",
    "resolve_hierarchy",
    "decode_xmp_packet",
    "report",
];

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
pub const DESCRIPTOR: &str = r#"{"owner":"STUDIO-MODULE-INTEROP_LRCAT","version":1,"scope":"fixed extraction only: no SQL engine, no query API, no writable catalog, no repair, no private database or cache; the decoder reads untrusted file bytes and emits typed import proposals plus provenance and loss receipts","public_entrypoints":["extract_catalog(Grant, Limits, CancellationToken) -> CatalogProposal","extract_library(Grant, Limits, LibraryLimits, CancellationToken) -> LibraryProposal","parse_smart_rules(bytes, RuleLimits) -> SmartRules","resolve_hierarchy([(id, parent)], max_depth) -> [HierarchyNode]","decode_xmp_packet(bytes, XmpLimits) -> XmpLibraryFields","report(result, correlation_id, revision, CancellationToken, Observe, SinkPort) -> Receipt"],"operator":{"intent":"Bring a Lightroom Classic catalog (.lrcat) into Studio as import proposals without trusting or modifying the file","result":"CatalogProposal: photos (path relative to root folder, capture time, rating, pick flag, color label, orientation), develop settings kept opaque and hashed, collections, keywords, per-entity counts where rows_seen == mapped + lost, and a loss list; nothing is committed","recovery":"ungranted_wal: grant the -wal sibling bytes or state that none exists; source_changed: re-grant stable immutable handles; encrypted_or_unrecognized / not_sqlite: the file is not a readable plain catalog; limit_*: raise the named limit or split the input; missing_required_table: the file is not a Lightroom Classic catalog; canceled: nothing was produced"},"model":{"grant":"Grant{main: ByteSource, wal: None|Granted(ByteSource)|PresentButNotGranted}; the caller states honestly whether a -wal sibling exists; PresentButNotGranted fails closed because the catalog could be stale","bytes":"SQLite file format header, table b-tree interior 0x05 and leaf 0x0d pages, varints, records, overflow pages, sqlite_master, committed-prefix WAL overlay; iterative walk with depth cap, visited-page cycle detection, overflow chain cap, page-read budget; limits are checked before the allocation or read they guard; an overflow page may be consumed by one chain only, so reuse by another row or table is refused as overflow_shared and a loop as overflow_cycle (test shared_overflow_chain_across_rows_is_refused); text kept in the schema, lookup tables and proposal is charged per copy against max_retained_text_bytes and exceeding it is limit_retained_text (test retained_text_budget_counts_every_copy); size fields at their numeric maximum are refused as typed errors (test sizes_at_numeric_maximum_are_typed_refusals)","accounting":"per entity: rows_seen is counted by the b-tree walker, mapped is the number of entries the proposal holds, lost is the number of rows that produced no entry (each a whole-row loss note, e.g. orphan_row or duplicate_rowid); field-level losses of a mapped row are listed separately; CatalogProposal::reconciles cross-checks the three against the proposal content and fails for a tampered proposal (test reconciliation_is_cross_checked_not_constructed)","tables_read":["AgLibraryRootFolder","AgLibraryFolder","AgLibraryFile","Adobe_images (required)","Adobe_imageDevelopSettings","AgLibraryCollection","AgLibraryCollectionImage","AgLibraryKeyword","AgLibraryKeywordImage","Adobe_variablesTable"],"tables_counted_as_loss":["AgLibraryCollectionContent","Adobe_AdditionalMetadata","AgHarvestedExifMetadata","Adobe_libraryImageDevelopHistoryStep","Adobe_libraryImageDevelopSnapshot"],"fingerprint":"main_len, wal_len, committed_wal_len, SHA-256 of main and WAL taken before and after decoding; a difference is source_changed","wal":"only frames up to the last valid commit frame (salt and checksum chain) are overlaid; an uncommitted or invalid tail is ignored and listed as a loss","dispositions":"editable, translated, approximate, opaque, unsupported, missing"},"limits":{"max_main_bytes":1073741824,"max_wal_bytes":268435456,"max_pages":2097152,"max_page_reads":16000000,"max_rows":4000000,"max_cells":16000000,"max_payload":16777216,"max_overflow_pages":4096,"max_btree_depth":64,"max_columns":2048,"max_unmapped_tables":256,"max_retained_opaque_bytes":268435456,"max_retained_text_bytes":536870912},"safety":"the library opens no path and writes nothing: the granted bytes hash identically before and after success and failure, and no SQL engine appears in the manifest or sources (test cancel_stale_no_source_write_no_sql_closure); decoded rows are proposals, not authority","cancel":"CancellationToken is checked per page read, per row and per hash chunk; a canceled call returns canceled and no partial proposal","undo":"proposals only; the host records an applied revision as one reversible step","argus":{"inspect":"CatalogProposal losses, counts and fingerprint","action":"extract_catalog with the same grant","capture":"caller captures the granted bytes hash, the proposal and the Observe frame"},"diagnostics":"report() emits one Observe terminal outcome; no ids, paths or row text are serialized by this crate","unverified":["Lightroom table and column names are taken from the module brief and a concept reference, not yet checked against a Lightroom-written catalog","Adobe_variablesTable name Adobe_DBVersion as the schema version carrier","AgLibraryCollection.creationId values for smart collections","community mapping of Camera Raw ProcessVersion internal numbers"],"notice":"parser success and row counts are not full conversion: every unmapped, opaque or unsupported item is listed in losses","develop_mapping":"LibraryProposal.develop: Exposure2012 (stops, |x|<=5), Contrast/Highlights/Shadows/Whites/Blacks2012, Vibrance, Saturation, Clarity2012 (|x|<=100) and Custom Temperature/Tint accepted by develop white_xy map into develop BasicTone, Presence and AbsoluteWhiteBalance; out of range, non-numeric, preset white balance and other keys are typed losses, every mapped row carries develop_scale_unverified (test develop_settings_and_zlib_xmp_map_typed)","xmp_inflate":"zlib values (bare or 4-byte length prefixed) inflate through hsk-studio-package inflate_exact capped at XmpLimits.max_input_bytes, with zlib header, preset-dictionary, Adler-32 and declared-length checks (same test)","ckc_import":"plan_ckc_import(LibraryProposal, resolver, CkcLimits) -> CkcImportPlan and apply_ckc_import(port, actor, plan): synonyms, collections parents first (non-set parents dropped as losses, smart collections only when the whole rule translates), membership and per-asset rating/flag/label/keyword mutations through the kernel-bindings CkcCatalogPort; idempotent request ids replay on re-apply (test library_proposal_imports_through_ckc_port_double, against the test-only double)","library":{"owner_boundary":"import proposals for the CKC catalog owner (STU-ASSET-001/004/019, STU-IO-189); no catalog, store, index, query or rule evaluation in this crate","result":"LibraryProposal: catalog (equal to extract_catalog for the same grant, test library_mapping_keeps_catalog_and_types_state), typed pick flag, colour label (default five names, other text Custom + color_label_custom_text), rating 0..5, collection kind (collection, smart collection, collection set), validated collection and keyword forests, keyword synonyms, smart-collection rule trees; library losses and counts for AgLibraryCollectionContent and AgLibraryKeywordSynonym; resolves lists catalog loss codes the library layer translated","hierarchy":"resolve_hierarchy is O(n): parent cycles and self-parents are detached as parent_cycle, unknown parents parent_unresolved, repeated ids duplicate_rowid, nesting beyond max_hierarchy_depth hierarchy_depth_exceeded (test hierarchy_cycles_unresolved_and_depth_cap)","rules":"parse_smart_rules tokenizes the Lua table literal (escapes decoded, long brackets, comments) without evaluating it; input length, nesting depth, node count and string length are refused as typed rule errors before the allocation each guards, and a refused row is a whole-row loss named by the rule error code (tests smart_rules_typed_tree_and_adversarial_text, smart_rule_bombs_refused_before_allocation)","xmp":"decode_xmp_packet reads xmp:Rating (-1 rejected, 0..5), xmp:Label, dc:subject and lr:hierarchicalSubject from an RDF/XML packet: references decoded before comparison, comments/PI/CDATA recognised, names resolved by namespace URI not prefix, DOCTYPE refused; input size, UTF-8, depth, node count, keyword count and value length are typed refusals before the allocation each guards (tests xmp_fields_namespace_and_adversarial_xml, xmp_bombs_refused_before_allocation); extract_library decodes plain-text Adobe_AdditionalMetadata packets per image, compressed-looking values are xmp_compressed_not_inflated (no inflate in this crate), other values xmp_not_packet, a rating that disagrees with the catalog is xmp_rating_differs_from_catalog; read only, XMP write is the CKC owner","unverified":["smart rule criteria, operation and combine names","Adobe_AdditionalMetadata.xmp storage form (plain packet or length-prefixed zlib)","lr:hierarchicalSubject separator |","creationId values com.adobe.ag.library.smart_collection and com.adobe.ag.library.group","owning module ag.library.smart_collection","table AgLibraryKeywordSynonym(keyword, name)","colour labels stored as label names of the active label set"]},"next":"WAL-aware multi-snapshot receipts, XMP inflate with an output cap, develop settings semantics, CKC promotion through shared ports"}"#;
