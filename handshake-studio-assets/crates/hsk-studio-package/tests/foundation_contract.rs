use hsk_studio_accord::CancellationToken;
use hsk_studio_folio as folio;
use hsk_studio_package::*;
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    io,
    path::{Path, PathBuf},
};

fn bounded_case(name: &'static str, body: impl FnOnce() + Send + 'static) {
    use std::{
        panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
        sync::mpsc,
        time::Duration,
    };
    let (sender, receiver) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let outcome = catch_unwind(AssertUnwindSafe(body));
        let _ = sender.send(outcome);
    });
    match receiver.recv_timeout(Duration::from_secs(30)) {
        Ok(Ok(())) => {}
        Ok(Err(p)) => resume_unwind(p),
        Err(mpsc::RecvTimeoutError::Timeout) => {
            panic!("case {name} exceeded declared 30-second deadline")
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            panic!("case {name} lost deadline worker result")
        }
    }
}

// ----- folio fixtures (same shape as the folio foundation contract) --------------------------

fn identity(prefix: &str, n: u8) -> String {
    format!("{prefix}-019abcde-0000-7000-8000-{n:012x}")
}
fn address(n: u8, op: &str, port: &str) -> Value {
    json!({"layer_id":identity("SLYR",n),"operation_key":op,"port_key":port})
}
fn source(n: u8, ty: &str) -> Value {
    json!({"layer_id":identity("SLYR",n),"operation_key":"source","operation_kind":"source","inputs":[],"outputs":[{"port_key":"out","value_type":ty,"cardinality":"one","semantic":"source"}]})
}
fn composite(n: u8) -> Value {
    json!({"layer_id":identity("SLYR",n),"operation_key":"mix","operation_kind":"composite","inputs":[{"port_key":"inputs","value_type":"image","cardinality":"many","semantic":"ordered_composite"}],"outputs":[{"port_key":"out","value_type":"image","cardinality":"one","semantic":"composite_result"}]})
}
fn layer(n: u8, kind: &str, payload: Value) -> Value {
    json!({"schema_id":"hsk.studio.layer@1","layer_id":identity("SLYR",n),"name":format!("layer-{n}"),"parent":null,"kind":kind,"payload":payload,"visible":true})
}
fn edge(source: Value, target: Value, n: u64) -> Value {
    json!({"source":source,"target":target,"dependency_kind":"data","input_ordinal":n})
}
fn tile(n: u8, digest: &ContentDigestPair) -> Value {
    json!({"object_key":format!("tile-{n}"),"layer_id":identity("SLYR",2),"column":n,"row":0,"width":{"value":64,"unit":"px"},"height":{"value":64,"unit":"px"},"format":"caller-selected-format","colour_profile_id":identity("SCPF",1),"schema_id":"hsk.studio.raster_tile@1","content_digest":{"algorithm":"sha256","digest":digest.0},"artifact_manifest_id":"caller-owned-existing-handle"})
}
struct ContentDigestPair(String);

fn mixed(tiles: Vec<Value>) -> Value {
    json!({
        "schema_id":"hsk.studio.document@1","document_id":identity("SDOC",1),"revision":7,"geometry_unit":"mm","colour_mode":3,"bits_per_channel":8,
        "working_space_rgb":null,"working_space_cmyk":null,"working_space_gray":null,"working_space_spot":null,"blending_space":null,
        "artboards":[{"schema_id":"hsk.studio.artboard@1","artboard_id":identity("SART",1),"name":"board","parent":null,"bounds":{"x":{"value":-1,"unit":"mm"},"y":{"value":0,"unit":"mm"},"width":{"value":100,"unit":"mm"},"height":{"value":50,"unit":"mm"}}}],"page_spreads":[],
        "layers":[layer(1,"group",json!({"payload_kind":"group"})),layer(2,"raster",json!({"payload_kind":"raster","tiles":tiles})),layer(3,"raster",json!({"payload_kind":"raster","tiles":[]})),layer(4,"vector",json!({"payload_kind":"vector","path_ids":[identity("SVPT",1)]})),layer(5,"text",json!({"payload_kind":"text","story_id":identity("STXT",1)}))],
        "graph":{"schema_id":"hsk.studio.layer_graph@1","operations":[source(2,"image"),source(3,"image"),source(4,"vector"),source(5,"text"),composite(1)],"edges":[edge(address(3,"source","out"),address(1,"mix","inputs"),1),edge(address(2,"source","out"),address(1,"mix","inputs"),0)],"outputs":[address(1,"mix","out"),address(4,"source","out"),address(5,"source","out")]}
    })
}

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
    fn format_supported(&self, f: &str) -> bool {
        f == "caller-selected-format"
    }
    fn primitive_layer_binding(&self, _: &str, _: &str) -> bool {
        false
    }
}

fn token() -> CancellationToken {
    CancellationToken::default()
}

fn open_doc(v: &Value) -> folio::Snapshot {
    match folio::inspect_bytes(
        &serde_json::to_vec(v).unwrap(),
        folio::Budget::default(),
        &token(),
        &Granted,
    )
    .unwrap()
    {
        folio::Inspection::Editable(s) => s,
        folio::Inspection::ReadOnly(_) => panic!("known typed document became read-only"),
    }
}

/// Document with three tiles over two distinct blobs (tile 0 and 2 share one blob).
fn fixture() -> (folio::Snapshot, MemoryAssetSource, Vec<Vec<u8>>) {
    let mut assets = MemoryAssetSource::new();
    let blob_a: Vec<u8> = (0..4096u32).map(|i| (i * 7 % 251) as u8).collect();
    let blob_b: Vec<u8> = (0..1500u32).map(|i| (i * 13 % 241) as u8).collect();
    let a = assets.insert(blob_a.clone());
    let b = assets.insert(blob_b.clone());
    let (a, b) = (ContentDigestPair(a.digest), ContentDigestPair(b.digest));
    let doc = mixed(vec![tile(0, &a), tile(1, &b), tile(2, &a)]);
    (open_doc(&doc), assets, vec![blob_a, blob_b])
}

fn write_to_vec(snapshot: &folio::Snapshot, assets: &dyn AssetSource) -> Vec<u8> {
    let mut out = Vec::new();
    write_package(snapshot, assets, &Limits::default(), &token(), &mut out).unwrap();
    out
}

fn read(bytes: &[u8]) -> Result<Opened, PackageError> {
    read_package(
        bytes,
        &Limits::default(),
        folio::Budget::default(),
        &Granted,
        &token(),
    )
}

// ----- independent raw ZIP builder (can emit hostile forms the writer never would) -----------

struct Raw<'a> {
    name: &'a str,
    method: u16,
    data: &'a [u8],
    declared: Option<u32>,
}
fn stored<'a>(name: &'a str, data: &'a [u8]) -> Raw<'a> {
    Raw {
        name,
        method: 0,
        data,
        declared: None,
    }
}
fn raw_zip(entries: &[Raw<'_>]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut cd = Vec::new();
    for e in entries {
        let offset = out.len() as u32;
        let crc = crc32fast::hash(e.data);
        let size = e.declared.unwrap_or(e.data.len() as u32);
        for (sig, local) in [(0x0403_4b50u32, true), (0x0201_4b50u32, false)] {
            let buf = if local { &mut out } else { &mut cd };
            buf.extend_from_slice(&sig.to_le_bytes());
            if !local {
                buf.extend_from_slice(&45u16.to_le_bytes());
            }
            buf.extend_from_slice(&20u16.to_le_bytes());
            buf.extend_from_slice(&0x0800u16.to_le_bytes());
            buf.extend_from_slice(&e.method.to_le_bytes());
            buf.extend_from_slice(&0u16.to_le_bytes());
            buf.extend_from_slice(&0x0021u16.to_le_bytes());
            buf.extend_from_slice(&crc.to_le_bytes());
            buf.extend_from_slice(&size.to_le_bytes());
            buf.extend_from_slice(&size.to_le_bytes());
            buf.extend_from_slice(&(e.name.len() as u16).to_le_bytes());
            buf.extend_from_slice(&0u16.to_le_bytes());
            if !local {
                buf.extend_from_slice(&[0u8; 2 + 2 + 2 + 4]);
                buf.extend_from_slice(&offset.to_le_bytes());
            }
            buf.extend_from_slice(e.name.as_bytes());
            if local {
                buf.extend_from_slice(e.data);
            }
        }
    }
    let cd_offset = out.len() as u32;
    out.extend_from_slice(&cd);
    out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    out.extend_from_slice(&[0, 0, 0, 0]);
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(cd.len() as u32).to_le_bytes());
    out.extend_from_slice(&cd_offset.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

/// Unpacks a valid package, lets `edit` change the (name, bytes) list, re-emits it with fresh CRCs.
fn repack(bytes: &[u8], edit: impl FnOnce(&mut Vec<(String, Vec<u8>)>)) -> Vec<u8> {
    let metas = preflight(bytes, &Limits::default(), &token()).unwrap();
    let mut entries: Vec<(String, Vec<u8>)> = metas
        .iter()
        .map(|m| (m.name.clone(), m.data(bytes).to_vec()))
        .collect();
    edit(&mut entries);
    let raw: Vec<Raw<'_>> = entries.iter().map(|(n, d)| stored(n, d)).collect();
    raw_zip(&raw)
}

fn manifest_of(entries: &[(String, Vec<u8>)]) -> Manifest {
    let (_, bytes) = entries.iter().find(|(n, _)| n == MANIFEST_NAME).unwrap();
    Manifest::parse(bytes, &Limits::default()).unwrap()
}
fn set_manifest(entries: &mut [(String, Vec<u8>)], manifest: &Manifest) {
    let slot = entries
        .iter_mut()
        .find(|(n, _)| n == MANIFEST_NAME)
        .unwrap();
    slot.1 = manifest.to_canonical_bytes().unwrap();
}

// ----- 1. mixed graph + bytes roundtrip ----------------------------------------------------

#[test]
fn mixed_graph_roundtrip_with_assets() {
    bounded_case("mixed_graph_roundtrip_with_assets", || {
        let (snapshot, assets, blobs) = fixture();
        let first = write_to_vec(&snapshot, &assets);
        assert_eq!(
            first,
            write_to_vec(&snapshot, &assets),
            "deterministic writer"
        );

        let metas = preflight(&first, &Limits::default(), &token()).unwrap();
        assert_eq!(metas.len(), 4, "manifest, document, two deduplicated blobs");
        assert_eq!(metas[0].name, MANIFEST_NAME);

        let opened = read(&first).unwrap();
        let reopened = opened.snapshot().expect("editable");
        assert_eq!(reopened.encoded_bytes(), snapshot.encoded_bytes());
        assert_eq!(opened.assets.len(), 2);
        for blob in &blobs {
            assert_eq!(opened.assets.get(&sha256_hex_of(blob)), Some(blob));
        }
        assert!(opened.quarantined.is_empty() && opened.loss.is_empty());

        let again = write_to_vec(reopened, &MemoryAssetSource::from_opened(&opened));
        assert_eq!(
            again, first,
            "second write of the opened result is byte-identical"
        );

        // Forced ZIP64 end records and headers exercise the reader's ZIP64 path for real.
        let mut forced = Vec::new();
        let receipt = write_package_with(
            &snapshot,
            &assets,
            &Extras::default(),
            &WriteOptions {
                zip64: Zip64Policy::Always,
                ..WriteOptions::default()
            },
            &Limits::default(),
            &token(),
            &mut forced,
        )
        .unwrap();
        assert!(receipt.zip64 && forced != first);
        let from_forced = read(&forced).unwrap();
        assert_eq!(from_forced.assets, opened.assets);
        assert_eq!(
            from_forced.snapshot().unwrap().encoded_bytes(),
            snapshot.encoded_bytes()
        );
        // ZIP64 EOCD32 offset field disagreeing with the ZIP64 record is rejected.
        let mut lying = forced.clone();
        let n = lying.len();
        lying[n - 6..n - 2].copy_from_slice(&0u32.to_le_bytes());
        assert_eq!(read(&lying).unwrap_err().code(), "malformed_archive");
    });
}

// ----- 2. hostile ZIP inputs ----------------------------------------------------------------

#[test]
fn hostile_zip_rejected() {
    bounded_case("hostile_zip_rejected", || {
        let limits = Limits::default();
        let code =
            |bytes: &[u8], limits: &Limits| preflight(bytes, limits, &token()).unwrap_err().code();
        let one = |name: &str| raw_zip(&[stored(name, b"x")]);
        assert_eq!(code(&one("../evil"), &limits), "traversal_name");
        assert_eq!(code(&one("a/../evil"), &limits), "traversal_name");
        assert_eq!(code(&one("/abs"), &limits), "absolute_name");
        assert_eq!(code(&one("C:/win"), &limits), "absolute_name");
        assert_eq!(code(&one("back\\slash"), &limits), "unsafe_name");
        assert_eq!(code(&one("CON.txt"), &limits), "unsafe_name");
        assert_eq!(code(&one("caf\u{e9}"), &limits), "unsafe_name");
        assert_eq!(
            code(
                &raw_zip(&[stored("a.bin", b"1"), stored("a.bin", b"2")]),
                &limits
            ),
            "duplicate_name"
        );
        assert_eq!(
            code(
                &raw_zip(&[stored("a.bin", b"1"), stored("A.BIN", b"2")]),
                &limits
            ),
            "case_fold_duplicate"
        );
        assert_eq!(
            code(&raw_zip(&[stored("a", b"1"), stored("a/b", b"2")]), &limits),
            "path_conflict"
        );
        let small = Limits {
            max_entry_bytes: 16,
            ..limits
        };
        assert_eq!(
            code(&raw_zip(&[stored("big.bin", &[0u8; 17])]), &small),
            "entry_too_large"
        );
        let lying = Raw {
            declared: Some(0x8000_0000),
            ..stored("lie.bin", b"x")
        };
        assert_eq!(code(&raw_zip(&[lying]), &limits), "entry_too_large");
        let deflated = Raw {
            method: 8,
            ..stored("bomb.bin", b"\x03\x00")
        };
        assert_eq!(
            code(&raw_zip(&[deflated]), &limits),
            "unsupported_compression"
        );
        let few = Limits {
            max_entries: 2,
            ..limits
        };
        assert_eq!(
            code(
                &raw_zip(&[stored("a", b"1"), stored("b", b"2"), stored("c", b"3")]),
                &few
            ),
            "too_many_entries"
        );
        let total = Limits {
            max_total_bytes: 4,
            ..limits
        };
        assert_eq!(
            code(
                &raw_zip(&[stored("a", b"123"), stored("b", b"456")]),
                &total
            ),
            "total_too_large"
        );

        // Layout differentials: prepended, trailing, local/central disagreement.
        let base = raw_zip(&[stored("a.bin", b"payload")]);
        let mut prepended = b"JUNK".to_vec();
        prepended.extend_from_slice(&base);
        assert_eq!(code(&prepended, &limits), "malformed_archive");
        let mut trailing = base.clone();
        trailing.extend_from_slice(b"tail");
        assert_eq!(code(&trailing, &limits), "malformed_archive");
        let mut renamed_local = base.clone();
        renamed_local[30] = b'b';
        assert_eq!(code(&renamed_local, &limits), "malformed_archive");
        let mut flipped = base.clone();
        flipped[30 + 5] ^= 1;
        assert_eq!(code(&flipped, &limits), "crc_mismatch");

        // Every truncation of a real package and every single-bit corruption is bounded and
        // panic-free; every truncation is rejected.
        let (snapshot, assets, _) = fixture();
        let package = write_to_vec(&snapshot, &assets);
        for end in 0..package.len() {
            assert!(read(&package[..end]).is_err(), "truncation at {end}");
        }
        for at in 0..package.len() {
            let mut damaged = package.clone();
            damaged[at] ^= 0x01;
            let _ = read(&damaged);
        }
    });
}

// ----- 3. manifest integrity, version, absent asset ----------------------------------------

#[test]
fn manifest_integrity_and_version() {
    bounded_case("manifest_integrity_and_version", || {
        let (snapshot, assets, _) = fixture();
        let package = write_to_vec(&snapshot, &assets);
        let code = |bytes: &[u8]| read(bytes).unwrap_err().code();
        let asset_index = |entries: &[(String, Vec<u8>)]| {
            entries
                .iter()
                .position(|(n, _)| n.starts_with(ASSET_PREFIX))
                .unwrap()
        };

        // Raw byte flip is caught by CRC; a consistent-CRC byte flip is caught by the manifest hash.
        let metas = preflight(&package, &Limits::default(), &token()).unwrap();
        let asset = metas
            .iter()
            .find(|m| m.name.starts_with(ASSET_PREFIX))
            .unwrap();
        let mut flipped = package.clone();
        let start = asset.data(&package).as_ptr() as usize - package.as_ptr() as usize;
        flipped[start] ^= 0xFF;
        assert_eq!(code(&flipped), "crc_mismatch");
        let hashed = repack(&package, |e| {
            let i = asset_index(e);
            e[i].1[0] ^= 0xFF;
        });
        assert_eq!(code(&hashed), "hash_mismatch");

        // Manifest length lie.
        let lied = repack(&package, |e| {
            let mut m = manifest_of(e);
            m.entries[0].len += 1;
            set_manifest(e, &m);
        });
        assert_eq!(code(&lied), "length_mismatch");

        // Unknown container version: reported before any other interpretation, no decode.
        let future = repack(&package, |e| {
            let slot = e.iter_mut().find(|(n, _)| n == MANIFEST_NAME).unwrap();
            slot.1 = br#"{"container_version":2,"future_field":[1,2,3]}"#.to_vec();
        });
        let err = read(&future).unwrap_err();
        assert_eq!(err, PackageError::UnsupportedVersion(2));

        // Non-canonical manifest bytes (valid JSON, extra whitespace) are rejected.
        let spaced = repack(&package, |e| {
            let slot = e.iter_mut().find(|(n, _)| n == MANIFEST_NAME).unwrap();
            slot.1.insert(1, b' ');
        });
        assert_eq!(code(&spaced), "manifest_non_canonical");

        // Manifest/document identity disagreement.
        let other_doc = repack(&package, |e| {
            let mut m = manifest_of(e);
            m.revision += 1;
            set_manifest(e, &m);
        });
        assert_eq!(code(&other_doc), "document_mismatch");

        // Referenced asset removed from both manifest and archive.
        let missing = repack(&package, |e| {
            let i = asset_index(e);
            let name = e.remove(i).0;
            let mut m = manifest_of(e);
            m.entries.retain(|x| x.name != name);
            set_manifest(e, &m);
        });
        assert_eq!(code(&missing), "missing_asset");

        // Manifest entry without an archive entry.
        let absent_entry = repack(&package, |e| {
            let i = asset_index(e);
            e.remove(i);
        });
        assert_eq!(code(&absent_entry), "missing_entry");

        // Absent asset in the grant: typed error, no partial output.
        let mut out = Vec::new();
        let err = write_package(
            &snapshot,
            &MemoryAssetSource::new(),
            &Limits::default(),
            &token(),
            &mut out,
        )
        .unwrap_err();
        assert_eq!(err, PackageError::Asset(AssetError::Absent));
        assert!(out.is_empty());
        assert_eq!(
            outcome_for::<()>(&Err(err)),
            hsk_studio_observe::Outcome::Failure(hsk_studio_observe::FailureCode::Unavailable)
        );
    });
}

// ----- 4. unknown records quarantined verbatim ----------------------------------------------

#[test]
fn unknown_record_quarantined_verbatim() {
    bounded_case("unknown_record_quarantined_verbatim", || {
        let (snapshot, assets, _) = fixture();
        let package = write_to_vec(&snapshot, &assets);
        let payload = b"MZ\x90\x00 not json, never interpreted \x00\xff".to_vec();
        let with_extra = repack(&package, |e| {
            e.push(("opaque/x.bin".to_owned(), payload.clone()))
        });

        let opened = read(&with_extra).unwrap();
        assert!(opened.snapshot().is_some());
        assert_eq!(opened.quarantined.len(), 1);
        assert_eq!(opened.quarantined[0].name, "opaque/x.bin");
        assert_eq!(opened.quarantined[0].bytes, payload);
        assert_eq!(opened.quarantined[0].sha256_hex, sha256_hex_of(&payload));
        assert_eq!(
            opened.loss,
            vec![LossNote {
                name: "opaque/x.bin".to_owned(),
                reason: LossReason::Unlisted
            }]
        );
        assert_eq!(
            outcome_for_opened(&Ok(opened.clone()), false),
            hsk_studio_observe::Outcome::Success
        );
        assert_eq!(
            outcome_for_opened(&Ok(opened.clone()), true),
            hsk_studio_observe::Outcome::Failure(hsk_studio_observe::FailureCode::Loss)
        );

        // Preserved on re-save as a listed, hash-verified opaque entry; stable thereafter.
        let mut resaved = Vec::new();
        write_package_with(
            opened.snapshot().unwrap(),
            &MemoryAssetSource::from_opened(&opened),
            &Extras {
                quarantined: &opened.quarantined,
                ..Extras::default()
            },
            &WriteOptions::default(),
            &Limits::default(),
            &token(),
            &mut resaved,
        )
        .unwrap();
        let second = read(&resaved).unwrap();
        assert_eq!(second.quarantined, opened.quarantined);
        assert_eq!(second.loss[0].reason, LossReason::Opaque);
        let mut third = Vec::new();
        write_package_with(
            second.snapshot().unwrap(),
            &MemoryAssetSource::from_opened(&second),
            &Extras {
                quarantined: &second.quarantined,
                ..Extras::default()
            },
            &WriteOptions::default(),
            &Limits::default(),
            &token(),
            &mut third,
        )
        .unwrap();
        assert_eq!(third, resaved);
    });
}

// ----- 5. atomic save: last-good retained, cancel, fault injection ------------------------

struct Fault {
    inner: StdFs,
    corrupt_readback: bool,
    fail_replace: bool,
}
impl AtomicFs for Fault {
    fn write_temp(&mut self, target: &Path, bytes: &[u8]) -> io::Result<PathBuf> {
        self.inner.write_temp(target, bytes)
    }
    fn read_back(&mut self, temp: &Path, max: u64) -> io::Result<Vec<u8>> {
        let mut bytes = self.inner.read_back(temp, max)?;
        if self.corrupt_readback {
            let mid = bytes.len() / 2;
            bytes[mid] ^= 0xFF;
        }
        Ok(bytes)
    }
    fn replace(&mut self, temp: &Path, target: &Path) -> io::Result<()> {
        if self.fail_replace {
            return Err(io::Error::other("injected replace failure"));
        }
        self.inner.replace(temp, target)
    }
    fn remove(&mut self, temp: &Path) {
        self.inner.remove(temp);
    }
}

/// Child-process fs: the process ends at `replace`, after the temp file is durable and verified,
/// exactly like a kill in that window (no destructors or cleanup run).
struct ExitAtReplace(StdFs);
impl AtomicFs for ExitAtReplace {
    fn write_temp(&mut self, target: &Path, bytes: &[u8]) -> io::Result<PathBuf> {
        self.0.write_temp(target, bytes)
    }
    fn read_back(&mut self, temp: &Path, max: u64) -> io::Result<Vec<u8>> {
        self.0.read_back(temp, max)
    }
    fn replace(&mut self, _: &Path, _: &Path) -> io::Result<()> {
        std::process::exit(86)
    }
    fn remove(&mut self, temp: &Path) {
        self.0.remove(temp);
    }
}

fn renamed_package() -> (Vec<u8>, Vec<u8>, Vec<Vec<u8>>) {
    let (snapshot, assets, blobs) = fixture();
    let old = write_to_vec(&snapshot, &assets);
    let renamed = snapshot
        .rename(
            &identity("SLYR", 2),
            "renamed".into(),
            7,
            8,
            folio::Budget::default(),
            &token(),
            &Granted,
        )
        .unwrap();
    (old, write_to_vec(&renamed, &assets), blobs)
}

struct TempDir(PathBuf);
impl TempDir {
    fn new(tag: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("hsk-studio-package-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
    fn listing(&self) -> BTreeSet<String> {
        std::fs::read_dir(&self.0)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect()
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn atomic_save_retains_last_good_and_cancel() {
    bounded_case("atomic_save_retains_last_good_and_cancel", || {
        let dir = TempDir::new("atomic");
        let target = dir.0.join("doc.handshake");
        let only_target: BTreeSet<String> = ["doc.handshake".to_owned()].into();
        let limits = Limits::default();
        let (old, new, _) = renamed_package();
        assert_ne!(old, new);

        save_atomic(&target, &old, &limits, &token()).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), old);
        assert_eq!(dir.listing(), only_target);

        let last_good_kept = |label: &str| {
            assert_eq!(
                std::fs::read(&target).unwrap(),
                old,
                "{label}: last-good changed"
            );
            assert_eq!(dir.listing(), only_target, "{label}: temp left behind");
        };

        let mut corrupt = Fault {
            inner: StdFs::new(),
            corrupt_readback: true,
            fail_replace: false,
        };
        assert!(save_atomic_with(&mut corrupt, &target, &new, &limits, &token()).is_err());
        last_good_kept("corrupt readback");

        let mut failing = Fault {
            inner: StdFs::new(),
            corrupt_readback: false,
            fail_replace: true,
        };
        assert_eq!(
            save_atomic_with(&mut failing, &target, &new, &limits, &token())
                .unwrap_err()
                .code(),
            "io"
        );
        last_good_kept("failed replace");

        let canceled = token();
        canceled.cancel();
        assert_eq!(
            save_atomic(&target, &new, &limits, &canceled).unwrap_err(),
            PackageError::Canceled
        );
        last_good_kept("pre-cancelled");

        assert!(save_atomic(&target, b"not a package", &limits, &token()).is_err());
        last_good_kept("invalid package");

        std::fs::create_dir(dir.0.join("sub")).unwrap();
        assert_eq!(
            save_atomic(&dir.0.join("sub"), &new, &limits, &token()).unwrap_err(),
            PackageError::DestinationInvalid
        );
        std::fs::remove_dir(dir.0.join("sub")).unwrap();

        save_atomic(&target, &new, &limits, &token()).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), new);
        assert_eq!(dir.listing(), only_target);
    });
}

// ===== slice 2: blob table (raw|zstd), streaming reader, stale-temp sweep =====================

/// Document with three tiles over three caller-chosen blobs.
fn fixture_blobs(blobs: &[Vec<u8>]) -> (folio::Snapshot, MemoryAssetSource) {
    let mut assets = MemoryAssetSource::new();
    let digests: Vec<ContentDigestPair> = blobs
        .iter()
        .map(|b| ContentDigestPair(assets.insert(b.clone()).digest))
        .collect();
    let tiles = digests
        .iter()
        .enumerate()
        .map(|(i, d)| tile(i as u8, d))
        .collect();
    (open_doc(&mixed(tiles)), assets)
}

fn lcg_bytes(n: usize, mut seed: u64) -> Vec<u8> {
    (0..n)
        .map(|_| {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (seed >> 33) as u8
        })
        .collect()
}

fn zstd_options() -> WriteOptions {
    WriteOptions {
        asset_codec: Codec::Zstd,
        ..WriteOptions::default()
    }
}

fn write_with(
    snapshot: &folio::Snapshot,
    assets: &dyn AssetSource,
    options: &WriteOptions,
) -> Vec<u8> {
    let mut out = Vec::new();
    write_package_with(
        snapshot,
        assets,
        &Extras::default(),
        options,
        &Limits::default(),
        &token(),
        &mut out,
    )
    .unwrap();
    out
}

/// Replaces the asset `hash` with `stored` bytes declared as a zstd blob of `raw_len` bytes.
fn with_zstd_asset(package: &[u8], hash: &str, stored: &[u8], raw_len: u64) -> Vec<u8> {
    repack(package, |entries| {
        let name = format!("{ASSET_PREFIX}{hash}");
        entries.iter_mut().find(|(n, _)| *n == name).unwrap().1 = stored.to_vec();
        let mut manifest = manifest_of(entries);
        let row = manifest
            .entries
            .iter_mut()
            .find(|e| e.name == name)
            .unwrap();
        row.len = stored.len() as u64;
        row.codec = Codec::Zstd;
        row.raw_len = Some(raw_len);
        set_manifest(entries, &manifest);
    })
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

#[test]
fn blob_table_zstd_codec() {
    bounded_case("blob_table_zstd_codec", || {
        // Real frames from an independent encoder (libzstd 1.5.7): without and with content checksum.
        let plain = b"handshake-tile-".repeat(40);
        let frames = [
            "28b52ffd605801b500007868616e647368616b652d74696c652d010046a49a75",
            "28b52ffd645801b500007868616e647368616b652d74696c652d010046a49a7564590e0c",
        ];
        let (snapshot, assets) = fixture_blobs(std::slice::from_ref(&plain));
        let raw_package = write_to_vec(&snapshot, &assets);
        let hash = sha256_hex_of(&plain);
        for frame in frames {
            let package = with_zstd_asset(&raw_package, &hash, &unhex(frame), plain.len() as u64);
            let opened = read(&package).unwrap();
            assert_eq!(
                opened.assets.get(&hash),
                Some(&plain),
                "libzstd frame decodes"
            );
            let row = opened
                .manifest
                .entries
                .iter()
                .find(|e| e.kind == EntryKind::Asset)
                .unwrap();
            assert_eq!((row.codec, row.raw_len), (Codec::Zstd, Some(600)));
        }

        // Writer-side zstd: verified against libzstd decode of the same frames outside the suite
        // (7 inputs, 2026-10-11); here the package roundtrip, determinism and shrinkage.
        let compressible = b"studio-raster-tile-row ".repeat(900);
        let noise = lcg_bytes(1500, 7);
        let (snapshot, assets) =
            fixture_blobs(&[compressible.clone(), noise.clone(), plain.clone()]);
        let raw = write_to_vec(&snapshot, &assets);
        let packed = write_with(&snapshot, &assets, &zstd_options());
        assert!(packed.len() < raw.len() / 2, "compressible blobs shrink");
        assert_eq!(
            packed,
            write_with(&snapshot, &assets, &zstd_options()),
            "deterministic"
        );
        let opened = read(&packed).unwrap();
        for blob in [&compressible, &noise, &plain] {
            assert_eq!(opened.assets.get(&sha256_hex_of(blob)), Some(blob));
        }
        let codecs: Vec<Codec> = opened
            .manifest
            .entries
            .iter()
            .filter(|e| e.kind == EntryKind::Asset)
            .map(|e| e.codec)
            .collect();
        assert_eq!(codecs.iter().filter(|c| **c == Codec::Zstd).count(), 2);
        assert_eq!(
            codecs.iter().filter(|c| **c == Codec::Raw).count(),
            1,
            "noise stays raw"
        );
        // Re-saving the decoded result with the same option reproduces the package byte-for-byte.
        let resaved = write_with(
            opened.snapshot().unwrap(),
            &MemoryAssetSource::from_opened(&opened),
            &zstd_options(),
        );
        assert_eq!(resaved, packed);

        // Hostile compressed blobs, each rejected with a typed code and bounded work.
        let frame = unhex(frames[0]);
        let code = |package: &[u8]| read(package).unwrap_err().code();
        let n = plain.len() as u64;
        assert_eq!(
            code(&with_zstd_asset(&raw_package, &hash, &frame, n - 1)),
            "length_mismatch"
        );
        assert_eq!(
            code(&with_zstd_asset(&raw_package, &hash, &frame, n + 1)),
            "length_mismatch"
        );
        let mut trailing = frame.clone();
        trailing.push(0);
        assert_eq!(
            code(&with_zstd_asset(&raw_package, &hash, &trailing, n)),
            "malformed_archive"
        );
        let mut damaged = frame.clone();
        damaged[frame.len() / 2] ^= 0x55;
        assert!(read(&with_zstd_asset(&raw_package, &hash, &damaged, n)).is_err());
        let wrong_identity = repack(&with_zstd_asset(&raw_package, &hash, &frame, n), |e| {
            let mut m = manifest_of(e);
            m.entries
                .iter_mut()
                .find(|r| r.kind == EntryKind::Asset)
                .unwrap()
                .sha256_hex = "0".repeat(64);
            set_manifest(e, &m);
        });
        assert!(matches!(
            code(&wrong_identity),
            "hash_mismatch" | "manifest_invalid"
        ));
        // Expansion bomb: 100 RLE blocks of 128 KiB (12.5 MiB) from ~400 bytes, declared 1000 bytes.
        let mut bomb = vec![0x28, 0xb5, 0x2f, 0xfd, 0x00, 0x38];
        for i in 0..100 {
            let header = if i == 99 {
                0x0010_0003u32
            } else {
                0x0010_0002u32
            };
            bomb.extend_from_slice(&header.to_le_bytes()[..3]);
            bomb.push(0xAB);
        }
        assert_eq!(
            code(&with_zstd_asset(&raw_package, &hash, &bomb, 1000)),
            "length_mismatch"
        );
        // Window declared above the limit is refused before any allocation.
        let tight = Limits {
            max_window_bytes: 1024,
            ..Limits::default()
        };
        let err = read_package(
            &with_zstd_asset(&raw_package, &hash, &bomb, 1000),
            &tight,
            folio::Budget::default(),
            &Granted,
            &token(),
        )
        .unwrap_err();
        assert_eq!(err.code(), "decode_failed");
        // The codec is only legal on assets, and needs a declared decoded length.
        let on_document = repack(&raw_package, |e| {
            let mut m = manifest_of(e);
            let row = m
                .entries
                .iter_mut()
                .find(|r| r.kind == EntryKind::Document)
                .unwrap();
            row.codec = Codec::Zstd;
            row.raw_len = Some(1);
            set_manifest(e, &m);
        });
        assert_eq!(code(&on_document), "manifest_invalid");
        let no_len = repack(&raw_package, |e| {
            let mut m = manifest_of(e);
            m.entries
                .iter_mut()
                .find(|r| r.kind == EntryKind::Asset)
                .unwrap()
                .codec = Codec::Zstd;
            set_manifest(e, &m);
        });
        assert_eq!(code(&no_len), "manifest_invalid");
    });
}

/// `Read + Seek` wrapper recording the largest single read request.
struct Counting<R> {
    inner: R,
    max_request: usize,
}
impl<R: io::Read> io::Read for Counting<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.max_request = self.max_request.max(buf.len());
        self.inner.read(buf)
    }
}
impl<R: io::Seek> io::Seek for Counting<R> {
    fn seek(&mut self, pos: io::SeekFrom) -> io::Result<u64> {
        self.inner.seek(pos)
    }
}

/// Virtual source of `len` zero bytes with a fixed tail: no storage, so a huge claim costs nothing.
struct Virtual {
    pos: u64,
    len: u64,
    tail: Vec<u8>,
    max_request: usize,
}
impl io::Read for Virtual {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.max_request = self.max_request.max(buf.len());
        let tail_start = self.len - self.tail.len() as u64;
        let mut n = 0;
        while n < buf.len() && self.pos < self.len {
            buf[n] = if self.pos >= tail_start {
                self.tail[(self.pos - tail_start) as usize]
            } else {
                0
            };
            self.pos += 1;
            n += 1;
        }
        Ok(n)
    }
}
impl io::Seek for Virtual {
    fn seek(&mut self, pos: io::SeekFrom) -> io::Result<u64> {
        self.pos = match pos {
            io::SeekFrom::Start(p) => p,
            io::SeekFrom::End(d) => self.len.checked_add_signed(d).unwrap(),
            io::SeekFrom::Current(d) => self.pos.checked_add_signed(d).unwrap(),
        };
        Ok(self.pos)
    }
}

fn summary(result: Result<Opened, PackageError>) -> Result<(Vec<u8>, Vec<String>), &'static str> {
    result
        .map(|o| {
            (
                o.snapshot()
                    .map(|s| s.encoded_bytes().to_vec())
                    .unwrap_or_default(),
                o.assets.keys().cloned().collect(),
            )
        })
        .map_err(|e| e.code())
}

#[test]
fn streaming_reader_matches_slice_reader_and_bounds_reads() {
    bounded_case(
        "streaming_reader_matches_slice_reader_and_bounds_reads",
        || {
            let limits = Limits::default();
            let stream = |bytes: &[u8]| {
                read_package_stream(
                    io::Cursor::new(bytes.to_vec()),
                    &limits,
                    folio::Budget::default(),
                    &Granted,
                    &token(),
                )
            };

            // Multi-chunk package (2.4 MB incompressible blob + compressible ones) over a counting source.
            let big = lcg_bytes(2_400_000, 99);
            let (snapshot, assets) =
                fixture_blobs(&[big.clone(), b"row ".repeat(5000), lcg_bytes(900, 3)]);
            let package = write_with(&snapshot, &assets, &zstd_options());
            let mut counting = Counting {
                inner: io::Cursor::new(package.clone()),
                max_request: 0,
            };
            let streamed = read_package_stream(
                &mut counting,
                &limits,
                folio::Budget::default(),
                &Granted,
                &token(),
            )
            .unwrap();
            let sliced = read(&package).unwrap();
            assert_eq!(streamed.assets, sliced.assets);
            assert_eq!(streamed.assets.get(&sha256_hex_of(&big)), Some(&big));
            assert_eq!(
                streamed.snapshot().unwrap().encoded_bytes(),
                snapshot.encoded_bytes()
            );
            assert_eq!(streamed.manifest, sliced.manifest);
            assert!(
                counting.max_request <= 1 << 20 && counting.max_request < big.len(),
                "largest single read request was {}",
                counting.max_request
            );

            // Lazy reader: verify everything without retaining, then fetch one asset by identity.
            let mut reader =
                PackageReader::open(io::Cursor::new(package.clone()), &limits, &token()).unwrap();
            assert_eq!(reader.entries().len(), 5);
            reader.verify_all(&token()).unwrap();
            let hash = sha256_hex_of(&big);
            assert_eq!(reader.read_asset(&hash, &token()).unwrap(), big);
            assert_eq!(
                reader.read_asset(&"0".repeat(64), &token()).unwrap_err(),
                PackageError::MissingAsset
            );
            assert_eq!(reader.manifest().revision, 7);
            assert_eq!(
                preflight_stream(&mut io::Cursor::new(&package), &limits, &token())
                    .unwrap()
                    .len(),
                5
            );

            // One implementation: every truncation and single-bit corruption of a small package gives
            // the identical verdict from both readers.
            let (small_doc, small_assets, _) = fixture();
            let small = write_with(&small_doc, &small_assets, &zstd_options());
            for end in 0..small.len() {
                assert_eq!(
                    summary(read(&small[..end])),
                    summary(stream(&small[..end])),
                    "truncate {end}"
                );
            }
            for at in 0..small.len() {
                let mut damaged = small.clone();
                damaged[at] ^= 0x10;
                assert_eq!(
                    summary(read(&damaged)),
                    summary(stream(&damaged)),
                    "flip {at}"
                );
            }

            // Hostile length claims never allocate or scan: a 4 GiB source whose EOCD announces a
            // 4 GiB central directory of zeros dies at the first record with small bounded reads.
            let total = u64::from(u32::MAX);
            let mut eocd = Vec::new();
            eocd.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
            eocd.extend_from_slice(&[0, 0, 0, 0]);
            eocd.extend_from_slice(&60_000u16.to_le_bytes());
            eocd.extend_from_slice(&60_000u16.to_le_bytes());
            eocd.extend_from_slice(&((total - 22) as u32).to_le_bytes());
            eocd.extend_from_slice(&0u32.to_le_bytes());
            eocd.extend_from_slice(&0u16.to_le_bytes());
            let virtual_source = Virtual {
                pos: 0,
                len: total,
                tail: eocd,
                max_request: 0,
            };
            let mut hostile = virtual_source;
            let err = read_package_stream(
                &mut hostile,
                &limits,
                folio::Budget::default(),
                &Granted,
                &token(),
            )
            .unwrap_err();
            assert_eq!(err.code(), "malformed_archive");
            assert!(
                hostile.max_request <= 64 * 1024,
                "request {}",
                hostile.max_request
            );
            let mut empty = Virtual {
                pos: 0,
                len: 1 << 40,
                tail: vec![],
                max_request: 0,
            };
            assert_eq!(
                preflight_stream(&mut empty, &limits, &token())
                    .unwrap_err()
                    .code(),
                "malformed_archive"
            );
            assert!(empty.max_request <= 64);
        },
    );
}

const SWEEP_CHILD_ENV: &str = "HSK_PACKAGE_SWEEP_CHILD";

/// Child-process body for the sweep test: `crash` ends at the rename, `hold` keeps a save open.
fn sweep_child(spec: &str) -> ! {
    use std::io::{BufRead, Write};
    let (mode, dir) = spec.split_once(':').unwrap();
    let (_, new, _) = renamed_package();
    let target = Path::new(dir).join("doc.handshake");
    match mode {
        "crash" => {
            let _ = save_atomic_with(
                &mut ExitAtReplace(StdFs::new()),
                &target,
                &new,
                &Limits::default(),
                &token(),
            );
            std::process::exit(2)
        }
        _ => {
            let mut held = StdFs::new();
            held.write_temp(&target, &new).unwrap();
            println!("HSK-READY");
            std::io::stdout().flush().unwrap();
            let mut line = String::new();
            while std::io::stdin().lock().read_line(&mut line).unwrap_or(0) > 0 {
                line.clear();
            }
            std::process::exit(87)
        }
    }
}

fn run_child(mode: &str, dir: &Path) -> std::process::Command {
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "stale_temp_sweep_owner_marker",
            "--test-threads=1",
            "--nocapture",
        ])
        .env(SWEEP_CHILD_ENV, format!("{mode}:{}", dir.display()));
    command
}

#[test]
fn stale_temp_sweep_owner_marker() {
    if let Ok(spec) = std::env::var(SWEEP_CHILD_ENV) {
        sweep_child(&spec);
    }
    bounded_case("stale_temp_sweep_owner_marker", || {
        use std::io::BufRead;
        use std::process::Stdio;
        let dir = TempDir::new("sweep");
        let target = dir.0.join("doc.handshake");
        let limits = Limits::default();
        let (old, new, _) = renamed_package();
        save_atomic(&target, &old, &limits, &token()).unwrap();
        let sweep = || sweep_stale_temps(&target, &token()).unwrap();

        // Look-alikes that must never be touched: unmarked temp, garbage marker, and a
        // well-formed marker naming some other file.
        let plain = dir.0.join("doc.handshake.tmp-9-9");
        std::fs::write(&plain, b"user data").unwrap();
        let garbage_temp = dir.0.join("doc.handshake.tmp-8-8");
        std::fs::write(&garbage_temp, b"user data 2").unwrap();
        std::fs::write(dir.0.join("doc.handshake.tmp-8-8.owner"), b"not a marker").unwrap();
        let unrelated = dir.0.join("unrelated.txt");
        std::fs::write(&unrelated, b"unrelated").unwrap();
        std::fs::write(
            dir.0.join("doc.handshake.tmp-7-7.owner"),
            b"hsk-studio-package-temp 1\npid 7\ntemp unrelated.txt\n",
        )
        .unwrap();
        let decoys = dir.listing();

        // A real process ends between the durable temp and the rename.
        let status = run_child("crash", &dir.0)
            .stdout(Stdio::null())
            .status()
            .unwrap();
        assert_eq!(
            status.code(),
            Some(86),
            "child must end at the injected kill point"
        );
        assert_eq!(
            std::fs::read(&target).unwrap(),
            old,
            "last-good survives the kill"
        );
        let crashed: Vec<String> = dir.listing().difference(&decoys).cloned().collect();
        assert_eq!(
            crashed.len(),
            2,
            "interrupted temp plus its marker: {crashed:?}"
        );
        let crashed_temp = crashed
            .iter()
            .find(|n| !n.ends_with(".owner"))
            .unwrap()
            .clone();
        assert_eq!(
            std::fs::read(dir.0.join(&crashed_temp)).unwrap(),
            new,
            "interrupted temp is the complete, durable successor"
        );

        // A second process keeps a save open (owner lock held) while we sweep.
        let mut live = run_child("hold", &dir.0)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut lines = std::io::BufReader::new(live.stdout.take().unwrap());
        loop {
            let mut line = String::new();
            assert!(
                lines.read_line(&mut line).unwrap() > 0,
                "live child never became ready"
            );
            // libtest prints "test <name> ... " without a newline before the body runs.
            if line.contains("HSK-READY") {
                break;
            }
        }
        let before_sweep = dir.listing();
        let first = sweep();
        assert_eq!(
            first.removed,
            vec![crashed_temp.clone()],
            "only the dead owner's temp goes"
        );
        assert_eq!(first.skipped_live, 1, "live owner is left alone");
        assert_eq!(
            first.ignored, 2,
            "garbage marker and foreign marker are ignored"
        );
        let after_first: BTreeSet<String> = dir.listing();
        let live_files: Vec<&String> = before_sweep
            .difference(&decoys)
            .filter(|n| !crashed.contains(n))
            .collect();
        assert_eq!(live_files.len(), 2, "live temp plus marker: {live_files:?}");
        assert!(live_files.iter().all(|n| after_first.contains(*n)));
        assert!(
            decoys.iter().all(|n| after_first.contains(n)),
            "decoys untouched"
        );
        assert_eq!(std::fs::read(&target).unwrap(), old);

        // The live owner ends without cleanup; its lock dies with it and the next sweep reclaims it.
        drop(live.stdin.take());
        assert_eq!(live.wait().unwrap().code(), Some(87));
        let second = sweep();
        assert_eq!(second.removed.len(), 1);
        assert_eq!(second.skipped_live, 0);
        assert_eq!(dir.listing(), decoys, "only the original decoys remain");
        assert_eq!(std::fs::read(&plain).unwrap(), b"user data");
        assert_eq!(std::fs::read(&garbage_temp).unwrap(), b"user data 2");
        assert_eq!(std::fs::read(&unrelated).unwrap(), b"unrelated");

        // An in-flight save of this very process is never swept; cleanup removes temp and marker.
        let mut mine = StdFs::new();
        let temp = mine.write_temp(&target, &new).unwrap();
        let during = sweep();
        assert!(during.removed.is_empty() && during.skipped_live == 1);
        assert!(temp.exists());
        mine.remove(&temp);
        assert_eq!(dir.listing(), decoys);
        assert!(sweep().removed.is_empty());
        // Next save still publishes over last-good after all of the above.
        save_atomic(&target, &new, &limits, &token()).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), new);
    });
}
