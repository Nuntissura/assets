//! SMB-CONVERT-NATIVE CN-2: typed import entries (records, receipt, original source bytes) travel
//! under reserved `import/<role>.<sha256hex>` names, round-trip byte-identically, are not quarantined,
//! add no loss note, and hostile ones are typed refusals with no partial output.

use hsk_studio_accord::CancellationToken;
use hsk_studio_folio as folio;
use hsk_studio_package::*;
use serde_json::json;

struct Granted;
impl folio::Resolver for Granted {
    fn primitive(&self, _: &str, _: &str) -> folio::Resolution {
        folio::Resolution::Available
    }
    fn profile(&self, _: &str) -> folio::Resolution {
        folio::Resolution::Available
    }
    fn tile(&self, _: &folio::TileRef) -> folio::Resolution {
        folio::Resolution::Available
    }
    fn format_supported(&self, _: &str) -> bool {
        true
    }
    fn primitive_layer_binding(&self, _: &str, _: &str) -> bool {
        false
    }
}

fn token() -> CancellationToken {
    CancellationToken::default()
}

fn id(prefix: &str, n: u8) -> String {
    format!("{prefix}-019abcde-0000-7000-8000-{n:012x}")
}

/// One empty raster layer: no assets needed.
fn snapshot() -> folio::Snapshot {
    let doc = json!({
        "schema_id":"hsk.studio.document@1","document_id":id("SDOC",1),"revision":1,"geometry_unit":"px","colour_mode":2,"bits_per_channel":8,
        "working_space_rgb":null,"working_space_cmyk":null,"working_space_gray":null,"working_space_spot":null,"blending_space":null,
        "artboards":[{"schema_id":"hsk.studio.artboard@1","artboard_id":id("SART",1),"name":"canvas","parent":null,"bounds":{"x":{"value":0,"unit":"px"},"y":{"value":0,"unit":"px"},"width":{"value":4,"unit":"px"},"height":{"value":4,"unit":"px"}}}],"page_spreads":[],
        "layers":[{"schema_id":"hsk.studio.layer@1","layer_id":id("SLYR",1),"name":"Background","parent":null,"kind":"raster","payload":{"payload_kind":"raster","tiles":[]},"visible":true}],
        "graph":{"schema_id":"hsk.studio.layer_graph@1","operations":[{"layer_id":id("SLYR",1),"operation_key":"source","operation_kind":"source","inputs":[],"outputs":[{"port_key":"out","value_type":"image","cardinality":"one","semantic":"source"}]}],"edges":[],"outputs":[{"layer_id":id("SLYR",1),"operation_key":"source","port_key":"out"}]}
    });
    match folio::inspect_bytes(&serde_json::to_vec(&doc).unwrap(), folio::Budget::default(), &token(), &Granted).unwrap() {
        folio::Inspection::Editable(s) => s,
        folio::Inspection::ReadOnly(_) => panic!("read-only"),
    }
}

fn write(snapshot: &folio::Snapshot, held: &[Quarantined], limits: &Limits) -> (Result<WriteReceipt, PackageError>, Vec<u8>) {
    let mut out = Vec::new();
    let extras = Extras { quarantined: held, previews: &[] };
    let r = write_package_with(snapshot, &MemoryAssetSource::new(), &extras, &WriteOptions::default(), limits, &token(), &mut out);
    (r, out)
}

fn entries() -> Vec<ImportEntry> {
    vec![
        ImportEntry::new(ImportRole::Records, br#"{"schema_id":"hsk.studio.raster_records@1","layers":[]}"#.to_vec()),
        ImportEntry::new(ImportRole::Receipt, br#"{"loss":[{"code":"profile_assumed_srgb"}]}"#.to_vec()),
        ImportEntry::new(ImportRole::Original, b"8BPS\x00\x01original source bytes".to_vec()),
    ]
}

#[test]
fn import_entries_round_trip_verified_and_add_no_loss_note() {
    let snap = snapshot();
    let imports = entries();
    let unknown = Quarantined { name: "x-unknown".into(), bytes: b"opaque".to_vec(), sha256_hex: sha256_hex_of(b"opaque") };
    let mut held: Vec<Quarantined> = imports.iter().map(ImportEntry::to_opaque).collect();
    held.push(unknown.clone());
    let (receipt, bytes) = write(&snap, &held, &Limits::default());
    receipt.expect("written");
    let opened = read_package(&bytes, &Limits::default(), folio::Budget::default(), &Granted, &token()).expect("read");
    assert_eq!(opened.imports.len(), 3);
    for expected in &imports {
        let got = opened.import(expected.role).expect("role present");
        assert_eq!(got, expected, "byte-identical and hash-verified");
        assert_eq!(sha256_hex_of(&got.bytes), got.sha256_hex);
    }
    assert_eq!(opened.quarantined, vec![unknown.clone()], "only the unknown entry is quarantined");
    assert_eq!(opened.loss.len(), 1, "import entries add no loss note");
    assert_eq!(opened.loss[0].name, "x-unknown");
    // Re-save of an opened package re-emits them: the container is byte-identical.
    let (again, rewritten) = write(&snap, &opened.preserved_entries(), &Limits::default());
    again.expect("re-written");
    assert_eq!(rewritten, bytes);
    // A reader that does not know them (streaming reader path) keeps them as typed too.
    let streamed = read_package_stream(std::io::Cursor::new(&bytes), &Limits::default(), folio::Budget::default(), &Granted, &token())
        .expect("stream read");
    assert_eq!(streamed.imports, opened.imports);
}

#[test]
fn hostile_import_entries_are_typed_refusals_with_no_partial_output() {
    let snap = snapshot();
    let receipt = ImportEntry::new(ImportRole::Receipt, vec![b'{'; 4096]);
    // Oversized metadata entry: records and receipts are bounded like the manifest.
    let small = Limits { max_manifest_bytes: 1024, ..Limits::default() };
    let (r, out) = write(&snap, &[receipt.to_opaque()], &small);
    assert_eq!(r.unwrap_err(), PackageError::EntryTooLarge);
    assert!(out.is_empty(), "no partial output");
    // Name digest that is not the content hash.
    let mut wrong = receipt.to_opaque();
    wrong.name = format!("{IMPORT_PREFIX}receipt.{}", sha256_hex_of(b"other"));
    let (r, out) = write(&snap, &[wrong], &Limits::default());
    assert_eq!(r.unwrap_err(), PackageError::HashMismatch);
    assert!(out.is_empty());
    // Unknown role and malformed digest under the reserved prefix.
    for name in [format!("{IMPORT_PREFIX}notes.{}", receipt.sha256_hex), format!("{IMPORT_PREFIX}receipt.ABC")] {
        let mut bad = receipt.to_opaque();
        bad.name = name;
        let (r, out) = write(&snap, &[bad], &Limits::default());
        assert_eq!(r.unwrap_err(), PackageError::ManifestInvalid);
        assert!(out.is_empty());
    }
    // Reader side, before any body is decoded: a valid package read under a smaller metadata bound,
    // and a manifest whose import entry names another digest.
    let (ok, bytes) = write(&snap, &[receipt.to_opaque()], &Limits::default());
    ok.expect("written");
    let opened = read_package(&bytes, &Limits::default(), folio::Budget::default(), &Granted, &token()).unwrap();
    assert!(opened.manifest.to_canonical_bytes().unwrap().len() < 1024, "the manifest itself fits the bound");
    assert_eq!(read_package(&bytes, &small, folio::Budget::default(), &Granted, &token()).unwrap_err(), PackageError::EntryTooLarge);
    let mut manifest = read_package(&bytes, &Limits::default(), folio::Budget::default(), &Granted, &token()).unwrap().manifest;
    let entry = manifest.entries.iter_mut().find(|e| e.kind == EntryKind::Opaque).unwrap();
    entry.sha256_hex = sha256_hex_of(b"other");
    let forged = manifest.to_canonical_bytes().unwrap();
    assert_eq!(Manifest::parse(&forged, &Limits::default()).unwrap_err(), PackageError::HashMismatch);
}
