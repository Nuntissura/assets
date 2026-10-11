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
    let slot = entries.iter_mut().find(|(n, _)| n == MANIFEST_NAME).unwrap();
    slot.1 = manifest.to_canonical_bytes().unwrap();
}

// ----- 1. mixed graph + bytes roundtrip ----------------------------------------------------

#[test]
fn mixed_graph_roundtrip_with_assets() {
    bounded_case("mixed_graph_roundtrip_with_assets", || {
        let (snapshot, assets, blobs) = fixture();
        let first = write_to_vec(&snapshot, &assets);
        assert_eq!(first, write_to_vec(&snapshot, &assets), "deterministic writer");

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
        assert_eq!(again, first, "second write of the opened result is byte-identical");

        // Forced ZIP64 end records and headers exercise the reader's ZIP64 path for real.
        let mut forced = Vec::new();
        let receipt = write_package_with(
            &snapshot,
            &assets,
            &[],
            &WriteOptions {
                zip64: Zip64Policy::Always,
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
        let code = |bytes: &[u8], limits: &Limits| {
            preflight(bytes, limits, &token()).unwrap_err().code()
        };
        let one = |name: &str| raw_zip(&[stored(name, b"x")]);
        assert_eq!(code(&one("../evil"), &limits), "traversal_name");
        assert_eq!(code(&one("a/../evil"), &limits), "traversal_name");
        assert_eq!(code(&one("/abs"), &limits), "absolute_name");
        assert_eq!(code(&one("C:/win"), &limits), "absolute_name");
        assert_eq!(code(&one("back\\slash"), &limits), "unsafe_name");
        assert_eq!(code(&one("CON.txt"), &limits), "unsafe_name");
        assert_eq!(code(&one("caf\u{e9}"), &limits), "unsafe_name");
        assert_eq!(
            code(&raw_zip(&[stored("a.bin", b"1"), stored("a.bin", b"2")]), &limits),
            "duplicate_name"
        );
        assert_eq!(
            code(&raw_zip(&[stored("a.bin", b"1"), stored("A.BIN", b"2")]), &limits),
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
        assert_eq!(code(&raw_zip(&[deflated]), &limits), "unsupported_compression");
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
            code(&raw_zip(&[stored("a", b"123"), stored("b", b"456")]), &total),
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
        let asset = metas.iter().find(|m| m.name.starts_with(ASSET_PREFIX)).unwrap();
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
        let with_extra = repack(&package, |e| e.push(("opaque/x.bin".to_owned(), payload.clone())));

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
            &opened.quarantined,
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
            &second.quarantined,
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

struct TempDir(PathBuf);
impl TempDir {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("hsk-studio-package-{tag}-{}", std::process::id()));
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
        let (snapshot, assets, _) = fixture();
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
        let new = write_to_vec(&renamed, &assets);
        assert_ne!(old, new);

        save_atomic(&target, &old, &limits, &token()).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), old);
        assert_eq!(dir.listing(), only_target);

        let last_good_kept = |label: &str| {
            assert_eq!(std::fs::read(&target).unwrap(), old, "{label}: last-good changed");
            assert_eq!(dir.listing(), only_target, "{label}: temp left behind");
        };

        let mut corrupt = Fault {
            inner: StdFs,
            corrupt_readback: true,
            fail_replace: false,
        };
        assert!(save_atomic_with(&mut corrupt, &target, &new, &limits, &token()).is_err());
        last_good_kept("corrupt readback");

        let mut failing = Fault {
            inner: StdFs,
            corrupt_readback: false,
            fail_replace: true,
        };
        assert_eq!(
            save_atomic_with(&mut failing, &target, &new, &limits, &token()).unwrap_err().code(),
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
