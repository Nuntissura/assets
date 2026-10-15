//! Foundation contract of the Lightroom catalog byte decoder (MT-35562 case names). These tests
//! prove BYTE DECODING only: the fixtures are synthetic lrcat-shaped files written once by an
//! offline generator kept outside the product repository (location and SHA-256 in
//! tests/fixtures/PROVENANCE.json); expected values come from its in-memory model and from
//! Python's own SHA-256, never from this decoder. Hostile images are built by hand below.
use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};
use hsk_studio_interop_lrcat::{
    ByteSource, CatalogProposal, CollectionKind, DESCRIPTOR, Disposition, Grant, LimitKind,
    Limits, LrcatError, PUBLIC_ENTRYPOINTS, SliceSource, SourceError, WalGrant, extract_catalog,
    report,
};
use hsk_studio_observe::{
    Budget, DeliveryClass, DeliveryError, FailureCode, Observe, Outcome, SinkPort,
};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

const MAIN: &[u8] = include_bytes!("fixtures/synthetic-main.lrcat");
const WAL_MAIN: &[u8] = include_bytes!("fixtures/synthetic-wal.lrcat");
const WAL: &[u8] = include_bytes!("fixtures/synthetic-wal.lrcat-wal");
const WAL_UNCOMMITTED: &[u8] = include_bytes!("fixtures/synthetic-wal-uncommitted.lrcat-wal");
const MAIN_TSV: &str = include_str!("fixtures/synthetic-main.expected.tsv");
const WAL_TSV: &str = include_str!("fixtures/synthetic-wal.expected.tsv");
const PROVENANCE: &str = include_str!("fixtures/PROVENANCE.json");

fn bounded_case(name: &'static str, body: impl FnOnce() + Send + 'static) {
    use std::{
        panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
        sync::mpsc,
        time::Duration,
    };
    let (tx, rx) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let _ = tx.send(catch_unwind(AssertUnwindSafe(body)));
    });
    match rx.recv_timeout(Duration::from_secs(30)) {
        Ok(Ok(())) => {}
        Ok(Err(p)) => resume_unwind(p),
        Err(e) => panic!("case {name} exceeded/lost declared 30s deadline: {e}"),
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn sha_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

/// `(bytes, sha256)` recorded by the Python generator for a fixture file.
fn provenance(name: &str) -> (u64, String) {
    let at = PROVENANCE.find(&format!("\"{name}\"")).expect("fixture in provenance");
    let rest = &PROVENANCE[at..];
    let bytes_at = rest.find("\"bytes\": ").unwrap() + 9;
    let bytes: u64 = rest[bytes_at..]
        .split(|c: char| !c.is_ascii_digit())
        .next()
        .unwrap()
        .parse()
        .unwrap();
    let sha_at = rest.find("\"sha256\": \"").unwrap() + 11;
    (bytes, rest[sha_at..sha_at + 64].to_owned())
}

fn grant_main(bytes: &[u8]) -> Grant {
    Grant::main_only(Box::new(SliceSource::new(bytes.to_vec())))
}

fn grant_wal(main: &[u8], wal: &[u8]) -> Grant {
    Grant {
        main: Box::new(SliceSource::new(main.to_vec())),
        wal: WalGrant::Granted(Box::new(SliceSource::new(wal.to_vec()))),
    }
}

fn extract(grant: &Grant) -> Result<CatalogProposal, LrcatError> {
    extract_catalog(grant, &Limits::default(), &CancellationToken::default())
}

fn extract_with(bytes: &[u8], limits: Limits) -> Result<CatalogProposal, LrcatError> {
    extract_catalog(&grant_main(bytes), &limits, &CancellationToken::default())
}

fn rows<'a>(tsv: &'a str, kind: &str) -> Vec<Vec<&'a str>> {
    tsv.lines()
        .map(|l| l.split('\t').collect::<Vec<_>>())
        .filter(|f| f[0] == kind)
        .collect()
}

fn opt(s: &str) -> Option<String> {
    (s != "\\N").then(|| s.to_owned())
}

fn assert_matches_expected(p: &CatalogProposal, tsv: &str) {
    let photos = rows(tsv, "photo");
    assert_eq!(p.photos.len(), photos.len());
    for (got, f) in p.photos.iter().zip(&photos) {
        // photo, id, global, root, rel, name, rating, pick, label, capture, orient, format, w, h
        assert_eq!(got.local_id.to_string(), f[1]);
        assert_eq!(got.global_id.as_deref(), Some(f[2]));
        assert_eq!(got.root_path.as_deref(), Some(f[3]));
        assert_eq!(got.path_from_root.as_deref(), Some(f[4]));
        assert_eq!(got.file_name.as_deref(), Some(f[5]));
        assert_eq!(got.rating.map(|r| r.to_string()), opt(f[6]), "rating of {}", f[1]);
        assert_eq!(got.pick.map(|r| r.to_string()).as_deref(), Some(f[7]));
        assert_eq!(got.color_label.as_deref(), Some(f[8]));
        assert_eq!(got.capture_time.as_deref(), Some(f[9]));
        assert_eq!(got.orientation.as_deref(), Some(f[10]));
        assert_eq!(got.file_format.as_deref(), Some(f[11]));
        assert_eq!(got.width.map(|w| w.to_string()).as_deref(), Some(f[12]));
        assert_eq!(got.height.map(|w| w.to_string()).as_deref(), Some(f[13]));
    }
    let develop = rows(tsv, "develop");
    assert_eq!(p.develop.len(), develop.len());
    for (got, f) in p.develop.iter().zip(&develop) {
        // develop, image, process_version, len, sha256
        assert_eq!(got.image_local_id.to_string(), f[1]);
        assert_eq!(got.process_version.as_deref(), Some(f[2]));
        assert_eq!(got.settings.len.to_string(), f[3]);
        assert_eq!(hex(&got.settings.sha256), f[4]);
        let bytes = got.settings.bytes.as_ref().expect("retained within budget");
        assert_eq!(sha_hex(bytes), f[4]);
    }
    let kinds = |s: &str| match s {
        "standard" => CollectionKind::Standard,
        "smart" => CollectionKind::Smart,
        _ => CollectionKind::Other,
    };
    let collections = rows(tsv, "collection");
    assert_eq!(p.collections.len(), collections.len());
    for (got, f) in p.collections.iter().zip(&collections) {
        assert_eq!(got.local_id.to_string(), f[1]);
        assert_eq!(got.name.as_deref(), Some(f[2]));
        assert_eq!(got.kind, kinds(f[3]));
    }
    let members: Vec<_> = p.collection_members.iter().map(|m| (m.collection, m.image)).collect();
    let expected: Vec<(i64, i64)> = rows(tsv, "member")
        .iter()
        .map(|f| (f[1].parse().unwrap(), f[2].parse().unwrap()))
        .collect();
    assert_eq!(members, expected);
    let keywords: Vec<_> = p
        .keywords
        .iter()
        .map(|k| (k.local_id.to_string(), k.name.clone().unwrap(), k.parent.map(|x| x.to_string())))
        .collect();
    let expected: Vec<_> = rows(tsv, "keyword").iter().map(|f| (f[1].to_owned(), f[2].to_owned(), opt(f[3]))).collect();
    assert_eq!(keywords, expected);
    let assigned: Vec<_> = p.keyword_assignments.iter().map(|a| (a.image, a.keyword)).collect();
    let expected: Vec<(i64, i64)> = rows(tsv, "assign")
        .iter()
        .map(|f| (f[1].parse().unwrap(), f[2].parse().unwrap()))
        .collect();
    assert_eq!(assigned, expected);
    let count = |key: &str| -> u64 { rows(tsv, "count").iter().find(|f| f[1] == key).unwrap()[2].parse().unwrap() };
    let c = |e: &str| p.count(e).unwrap_or_else(|| panic!("count {e}"));
    assert_eq!(c("Adobe_images").rows_seen, count("photos"));
    assert_eq!(c("Adobe_imageDevelopSettings").rows_seen, count("develop_seen"));
    assert_eq!(c("Adobe_imageDevelopSettings").mapped, count("develop_mapped"));
    assert_eq!(c("AgLibraryCollectionImage").rows_seen, count("members_seen"));
    assert_eq!(c("AgLibraryCollectionImage").mapped, count("members_mapped"));
    assert_eq!(c("AgLibraryKeywordImage").rows_seen, count("assign_seen"));
    assert_eq!(c("AgLibraryKeywordImage").mapped, count("assign_mapped"));
    assert_eq!(p.schema_version.as_deref(), Some(rows(tsv, "schema_version")[0][1]));
    assert!(p.reconciles(), "rows_seen == mapped + lost for every entity: {:?}", p.counts);
}

#[test]
fn real_catalog_wal_fixed_entity_reconciliation() {
    bounded_case("real_catalog_wal_fixed_entity_reconciliation", || {
        // Main file alone: exact entities, reconciliation, independent fingerprint, stable re-read.
        let first = extract(&grant_main(MAIN)).expect("main fixture decodes");
        assert_matches_expected(&first, MAIN_TSV);
        let (len, sha) = provenance("synthetic-main.lrcat");
        assert_eq!((first.fingerprint.main_len, first.fingerprint.main_sha256_hex()), (len, sha));
        assert_eq!(first.fingerprint.wal_len, 0);
        assert_eq!(extract(&grant_main(MAIN)).unwrap(), first, "stable across two reads");
        assert!(first.loss("freelist", "freelist_count_mismatch").is_none(), "real freelist is consistent");
        assert!(!first.is_lossless() && first.notice.contains("not full conversion"));

        // Committed WAL overlays the main file: the WAL value wins, new rows appear.
        let stale = extract(&grant_main(WAL_MAIN)).expect("wal-mode main without its wal");
        assert_eq!(stale.photos.len(), 300);
        assert_eq!(stale.photos[0].rating, Some(1));
        let fresh = extract(&grant_wal(WAL_MAIN, WAL)).expect("main + committed wal");
        assert_matches_expected(&fresh, WAL_TSV);
        assert_eq!(fresh.photos.len(), 301);
        assert_eq!((fresh.photos[0].rating, fresh.photos[0].pick), (Some(5), Some(1)));
        assert_eq!(fresh.photos[300].file_name.as_deref(), Some("IMG_0301.CR2"));
        let (wal_len, wal_sha) = provenance("synthetic-wal.lrcat-wal");
        assert_eq!(fresh.fingerprint.wal_len, wal_len);
        assert_eq!(fresh.fingerprint.committed_wal_len, wal_len);
        assert_eq!(fresh.fingerprint.wal_sha256_hex(), wal_sha);
        assert_eq!((fresh.wal.committed_frames, fresh.wal.ignored_tail_frames), (9, 0));
        assert_eq!(extract(&grant_wal(WAL_MAIN, WAL)).unwrap(), fresh);
    });
}

// ---- hand-built hostile images ----
const PS: usize = 512;

enum V<'a> {
    Null,
    Int(i64),
    Text(&'a str),
}

fn varint(v: u64) -> Vec<u8> {
    let mut groups = vec![];
    let mut x = v;
    loop {
        groups.push((x & 0x7f) as u8);
        x >>= 7;
        if x == 0 {
            break;
        }
    }
    groups.reverse();
    let n = groups.len();
    for (i, g) in groups.iter_mut().enumerate() {
        if i + 1 < n {
            *g |= 0x80;
        }
    }
    groups
}

fn record(vals: &[V]) -> Vec<u8> {
    let (mut types, mut body) = (vec![], vec![]);
    for v in vals {
        match v {
            V::Null => types.extend(varint(0)),
            V::Int(i) => {
                types.extend(varint(6));
                body.extend(i.to_be_bytes());
            }
            V::Text(s) => {
                types.extend(varint(13 + 2 * s.len() as u64));
                body.extend(s.as_bytes());
            }
        }
    }
    let mut out = varint(types.len() as u64 + 1);
    out.extend(types);
    out.extend(body);
    out
}

fn leaf_cell(rowid: u64, payload: &[u8]) -> Vec<u8> {
    let mut c = varint(payload.len() as u64);
    c.extend(varint(rowid));
    c.extend(payload);
    c
}

fn pack(page_type: u8, hdr: usize, header_len: usize, cells: &[Vec<u8>], right: u32) -> Vec<u8> {
    let mut p = vec![0u8; PS];
    p[hdr] = page_type;
    p[hdr + 3..hdr + 5].copy_from_slice(&(cells.len() as u16).to_be_bytes());
    let mut end = PS;
    for (i, c) in cells.iter().enumerate() {
        end -= c.len();
        p[end..end + c.len()].copy_from_slice(c);
        let at = hdr + header_len + i * 2;
        p[at..at + 2].copy_from_slice(&(end as u16).to_be_bytes());
    }
    p[hdr + 5..hdr + 7].copy_from_slice(&(end as u16).to_be_bytes());
    if page_type == 0x05 {
        p[hdr + 8..hdr + 12].copy_from_slice(&right.to_be_bytes());
    }
    p
}

fn leaf_page(hdr: usize, cells: &[Vec<u8>]) -> Vec<u8> {
    pack(0x0d, hdr, 8, cells, 0)
}

fn interior_page(children: &[u32], right: u32) -> Vec<u8> {
    let cells: Vec<Vec<u8>> = children
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let mut cell = c.to_be_bytes().to_vec();
            cell.extend(varint(i as u64 + 1));
            cell
        })
        .collect();
    pack(0x05, 0, 12, &cells, right)
}

/// Page 1 (header + `sqlite_master` naming `Adobe_images` rooted at page 2) followed by `rest`.
fn image(rest: Vec<Vec<u8>>) -> Vec<u8> {
    image_with("Adobe_images", rest)
}

fn image_with(table: &str, rest: Vec<Vec<u8>>) -> Vec<u8> {
    image_rooted(table, 2, rest)
}

/// Like `image_with`, but `sqlite_master` declares `root` as the table's root page.
fn image_rooted(table: &str, root: i64, rest: Vec<Vec<u8>>) -> Vec<u8> {
    let sql = format!("CREATE TABLE {table} (id_local INTEGER PRIMARY KEY, id_global, rootFile, rating)");
    let master = record(&[V::Text("table"), V::Text(table), V::Text(table), V::Int(root), V::Text(&sql)]);
    let mut page1 = leaf_page(100, &[leaf_cell(1, &master)]);
    page1[..16].copy_from_slice(b"SQLite format 3\0");
    page1[16..18].copy_from_slice(&(PS as u16).to_be_bytes());
    page1[18] = 1;
    page1[19] = 1;
    page1[21..24].copy_from_slice(&[64, 32, 32]);
    page1[24..28].copy_from_slice(&1u32.to_be_bytes());
    page1[28..32].copy_from_slice(&(rest.len() as u32 + 1).to_be_bytes());
    page1[56..60].copy_from_slice(&1u32.to_be_bytes());
    page1[92..96].copy_from_slice(&1u32.to_be_bytes());
    let mut out = page1;
    for p in rest {
        out.extend(p);
    }
    out
}

/// A table-leaf cell whose declared payload is `total` bytes, spilling to overflow page 3.
fn spilling_cell(total: u64) -> Vec<u8> {
    spilling_cell_to(total, 3)
}

/// Like `spilling_cell`, with an explicit first-overflow-page pointer.
fn spilling_cell_to(total: u64, overflow_page_no: u32) -> Vec<u8> {
    let (u, x) = (PS as u64, PS as u64 - 35);
    let m = (u - 12) * 32 / 255 - 23;
    let k = m + (total - m) % (u - 4);
    let local = if k <= x { k } else { m } as usize;
    let mut cell = varint(total);
    cell.extend(varint(1));
    cell.extend(vec![0u8; local]);
    cell.extend(overflow_page_no.to_be_bytes());
    cell
}

fn overflow_page(next: u32) -> Vec<u8> {
    let mut p = vec![0u8; PS];
    p[..4].copy_from_slice(&next.to_be_bytes());
    p
}

fn row_image() -> Vec<u8> {
    let row = record(&[V::Null, V::Text("g"), V::Int(7), V::Int(3)]);
    image(vec![leaf_page(0, &[leaf_cell(1, &row)])])
}

fn empty_images() -> Vec<u8> {
    image(vec![leaf_page(0, &[])])
}

#[test]
fn overflow_cycle_ungranted_wal_and_changed_source() {
    bounded_case("overflow_cycle_ungranted_wal_and_changed_source", || {
        // Sanity: the hand-built image decodes and a row with no path tables is a typed loss.
        let ok = extract_with(&row_image(), Limits::default()).expect("hand-built image");
        assert_eq!(ok.photos.len(), 1);
        assert_eq!(ok.photos[0].rating, Some(3));
        assert!(ok.loss("Adobe_images", "file_unresolved").is_some());

        // b-tree cycle: interior page 2 -> 3 -> back to 2.
        let cycle = image(vec![interior_page(&[3], 3), interior_page(&[2], 2)]);
        assert_eq!(extract_with(&cycle, Limits::default()).unwrap_err(), LrcatError::Cycle("btree_cycle"));
        // Overflow chain loop, chain past the end of the file, and a chain that ends early.
        let looped = image(vec![leaf_page(0, &[spilling_cell(2000)]), overflow_page(3)]);
        assert_eq!(extract_with(&looped, Limits::default()).unwrap_err(), LrcatError::Cycle("overflow_cycle"));
        let beyond = image(vec![leaf_page(0, &[spilling_cell(2000)]), overflow_page(99)]);
        assert_eq!(extract_with(&beyond, Limits::default()).unwrap_err(), LrcatError::Corrupt("page_number"));
        let short = image(vec![leaf_page(0, &[spilling_cell(2000)]), overflow_page(0)]);
        assert_eq!(extract_with(&short, Limits::default()).unwrap_err(), LrcatError::Corrupt("overflow_chain_short"));
        // Declared sizes are refused BEFORE any allocation or chain walk.
        let huge = image(vec![leaf_page(0, &[spilling_cell(1 << 40)]), overflow_page(0)]);
        assert_eq!(extract_with(&huge, Limits::default()).unwrap_err(), LrcatError::Limit(LimitKind::Payload));
        let long = image(vec![leaf_page(0, &[spilling_cell(10_000_000)]), overflow_page(0)]);
        let few = Limits { max_overflow_pages: 8, ..Limits::default() };
        assert_eq!(extract_with(&long, few).unwrap_err(), LrcatError::Limit(LimitKind::OverflowPages));
        // Configured limits on the real fixture.
        let lim = |l: Limits| extract_with(MAIN, l).unwrap_err();
        let d = Limits::default;
        assert_eq!(lim(Limits { max_pages: 3, ..d() }), LrcatError::Limit(LimitKind::Pages));
        assert_eq!(lim(Limits { max_btree_depth: 1, ..d() }), LrcatError::Limit(LimitKind::Depth));
        assert_eq!(lim(Limits { max_rows: 10, ..d() }), LrcatError::Limit(LimitKind::Rows));
        assert_eq!(lim(Limits { max_cells: 5, ..d() }), LrcatError::Limit(LimitKind::Cells));
        assert_eq!(lim(Limits { max_page_reads: 5, ..d() }), LrcatError::Limit(LimitKind::PageReads));
        assert_eq!(lim(Limits { max_main_bytes: 1000, ..d() }), LrcatError::Limit(LimitKind::MainBytes));
        assert_eq!(lim(Limits { max_main_bytes: u64::MAX, ..d() }), LrcatError::Limit(LimitKind::Configuration));
        // Not a catalog, ciphertext-looking bytes, truncation, freelist anomaly.
        assert_eq!(extract_with(&b"hello world ".repeat(100), Limits::default()).unwrap_err(), LrcatError::NotSqlite);
        let mut x = 0x9e37_79b9_7f4a_7c15u64;
        let noise: Vec<u8> = (0..4096)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                (x >> 24) as u8
            })
            .collect();
        assert_eq!(extract_with(&noise, Limits::default()).unwrap_err(), LrcatError::Encrypted);
        assert_eq!(extract_with(&MAIN[..MAIN.len() / 2], Limits::default()).unwrap_err(), LrcatError::Corrupt("main_truncated"));
        let mut odd = MAIN.to_vec();
        odd[36..40].copy_from_slice(&(u32::from_be_bytes([MAIN[36], MAIN[37], MAIN[38], MAIN[39]]) + 5).to_be_bytes());
        let flagged = extract_with(&odd, Limits::default()).expect("freelist anomaly is a loss, not a failure");
        let note = flagged.loss("freelist", "freelist_count_mismatch").expect("typed freelist loss");
        assert_eq!(note.disposition, Disposition::Unsupported);

        // A WAL sibling that exists but is not granted fails closed.
        let ungranted = Grant { main: Box::new(SliceSource::new(WAL_MAIN.to_vec())), wal: WalGrant::PresentButNotGranted };
        assert_eq!(extract(&ungranted).unwrap_err(), LrcatError::UngrantedWal);
        // Uncommitted trailing WAL frames are ignored; a damaged commit frame drops the whole tx.
        let committed = extract(&grant_wal(WAL_MAIN, WAL)).unwrap();
        let tail = extract(&grant_wal(WAL_MAIN, WAL_UNCOMMITTED)).unwrap();
        assert_eq!(tail.photos, committed.photos);
        assert_eq!((tail.wal.committed_frames, tail.fingerprint.committed_wal_len), (9, committed.fingerprint.wal_len));
        assert!(tail.wal.ignored_tail_frames > 0 && tail.fingerprint.wal_len > tail.fingerprint.committed_wal_len);
        assert!(tail.loss("wal", "wal_uncommitted_tail_ignored").is_some());
        let mut damaged = WAL.to_vec();
        *damaged.last_mut().unwrap() ^= 0xff;
        let rolled_back = extract(&grant_wal(WAL_MAIN, &damaged)).unwrap();
        assert_eq!((rolled_back.photos.len(), rolled_back.photos[0].rating), (300, Some(1)));
        assert_eq!(rolled_back.wal.committed_frames, 0);
        assert!(rolled_back.loss("wal", "wal_invalid_tail_ignored").is_some());
        let mut bad_header = WAL.to_vec();
        bad_header[9] ^= 1;
        assert_eq!(extract(&grant_wal(WAL_MAIN, &bad_header)).unwrap_err(), LrcatError::Corrupt("wal_page_size"));

        // A source whose bytes change between the two fingerprint passes is rejected.
        struct Flaky {
            inner: SliceSource,
            full_reads: AtomicUsize,
        }
        impl ByteSource for Flaky {
            fn len(&self) -> u64 {
                self.inner.len()
            }
            fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<(), SourceError> {
                self.inner.read_at(offset, buf)?;
                if buf.len() as u64 == self.inner.len() && self.full_reads.fetch_add(1, Ordering::SeqCst) == 1 {
                    *buf.last_mut().unwrap() ^= 0xff;
                }
                Ok(())
            }
        }
        let flaky = Grant::main_only(Box::new(Flaky { inner: SliceSource::new(empty_images()), full_reads: AtomicUsize::new(0) }));
        assert_eq!(extract(&flaky).unwrap_err(), LrcatError::SourceChanged);
        assert!(extract(&grant_main(&empty_images())).is_ok());
    });
}

#[test]
fn process_enable_smart_rule_unknown_loss() {
    bounded_case("process_enable_smart_rule_unknown_loss", || {
        let p = extract(&grant_main(MAIN)).expect("main fixture decodes");
        let loss = |entity: &str, code: &str| {
            p.loss(entity, code).unwrap_or_else(|| panic!("loss {entity}/{code}: {:?}", p.losses))
        };
        // Develop settings stay opaque; unknown process version and Enable flags are listed.
        let dev = "Adobe_imageDevelopSettings";
        assert_eq!((loss(dev, "settings_lua_opaque").count, loss(dev, "settings_lua_opaque").disposition), (40, Disposition::Opaque));
        assert_eq!(loss(dev, "process_version_unrecognised").count, 1);
        assert_eq!(loss(dev, "process_version_unrecognised").disposition, Disposition::Unsupported);
        assert_eq!(loss(dev, "enable_flags_unmapped").count, 1);
        assert_eq!(loss(dev, "orphan_row").count, 1);
        let c = p.count(dev).unwrap();
        assert_eq!((c.rows_seen, c.mapped, c.lost), (41, 40, 1));
        let overflowing = p.develop.iter().find(|d| d.image_local_id == 3).unwrap();
        assert!(overflowing.settings.len > 4000, "decoded through an overflow chain");
        assert_eq!(p.develop[0].scalars.get("EnableToneCurve").map(String::as_str), Some("true"));
        assert_eq!(p.develop[1].process_version.as_deref(), Some("99.9"));
        // Smart collection rules and other collection kinds are not decoded.
        let col = "AgLibraryCollection";
        assert_eq!(loss(col, "smart_collection_rules_not_decoded").count, 1);
        assert_eq!(loss(col, "collection_kind_unmapped").count, 1);
        assert_eq!(loss("AgLibraryCollectionContent", "collection_content_not_decoded").count, 1);
        // Unknown table and count-only tables are never counted as mapped.
        let unknown = p.count("table:Hsk_Synthetic_Unknown").unwrap();
        assert_eq!((unknown.rows_seen, unknown.mapped, unknown.lost), (3, 0, 3));
        assert_eq!(loss("unmapped_table", "unmapped_table").count, 3);
        for (table, code, n) in [
            ("Adobe_AdditionalMetadata", "xmp_blob_not_inflated", 50),
            ("AgHarvestedExifMetadata", "exif_harvest_not_mapped", 20),
            ("Adobe_libraryImageDevelopHistoryStep", "develop_history_not_mapped", 5),
        ] {
            assert_eq!(loss(table, code).count, n);
            let c = p.count(table).unwrap();
            assert_eq!((c.rows_seen, c.mapped, c.lost), (n, 0, n));
        }
        assert_eq!(loss("catalog", "schema_version_unrecognised").disposition, Disposition::Unsupported);
        assert!(p.reconciles());
        // The opaque byte budget drops bytes, never the hash or the length.
        let tight = extract_with(MAIN, Limits { max_retained_opaque_bytes: 100, ..Limits::default() }).unwrap();
        assert!(tight.develop.iter().all(|d| d.settings.bytes.is_none() && d.settings.len > 0));
        assert_eq!(tight.develop[0].settings.sha256, p.develop[0].settings.sha256);
        assert!(tight.loss(dev, "opaque_bytes_not_retained").is_some());
        // A catalog without Adobe_images is not a Lightroom catalog.
        let no_images = image_with("Something_else", vec![leaf_page(0, &[])]);
        assert_eq!(extract_with(&no_images, Limits::default()).unwrap_err(), LrcatError::MissingRequiredTable("Adobe_images"));
    });
}

#[test]
fn cancel_stale_no_source_write_no_sql_closure() {
    bounded_case("cancel_stale_no_source_write_no_sql_closure", || {
        // Pre-canceled token: no proposal.
        let token = CancellationToken::default();
        token.cancel();
        let err = extract_catalog(&grant_main(MAIN), &Limits::default(), &token).unwrap_err();
        assert_eq!((err.clone(), err.failure_code()), (LrcatError::Canceled, None));
        // Cancel in the middle of the hash pass and in the middle of the walk.
        struct Tripwire {
            inner: SliceSource,
            reads: AtomicUsize,
            trip_at: usize,
            token: CancellationToken,
        }
        impl ByteSource for Tripwire {
            fn len(&self) -> u64 {
                self.inner.len()
            }
            fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<(), SourceError> {
                if self.reads.fetch_add(1, Ordering::SeqCst) + 1 == self.trip_at {
                    self.token.cancel();
                }
                self.inner.read_at(offset, buf)
            }
        }
        for trip_at in [2, 3, 40, 150] {
            let token = CancellationToken::default();
            let src = Tripwire { inner: SliceSource::new(MAIN.to_vec()), reads: AtomicUsize::new(0), trip_at, token: token.clone() };
            let grant = Grant::main_only(Box::new(src));
            assert_eq!(extract_catalog(&grant, &Limits::default(), &token).unwrap_err(), LrcatError::Canceled, "trip_at {trip_at}");
        }
        // The granted bytes are only read: shared buffer identical before and after, even on error.
        struct Shared(Arc<Vec<u8>>);
        impl ByteSource for Shared {
            fn len(&self) -> u64 {
                self.0.len() as u64
            }
            fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<(), SourceError> {
                let start = offset as usize;
                buf.copy_from_slice(self.0.get(start..start + buf.len()).ok_or(SourceError::OutOfRange)?);
                Ok(())
            }
        }
        let shared = Arc::new(MAIN.to_vec());
        let before = sha_hex(&shared);
        extract(&Grant::main_only(Box::new(Shared(shared.clone())))).unwrap();
        assert!(extract_catalog(&Grant::main_only(Box::new(Shared(shared.clone()))), &Limits { max_rows: 10, ..Limits::default() }, &CancellationToken::default()).is_err());
        assert_eq!(sha_hex(&shared), before);
        assert_eq!(before, provenance("synthetic-main.lrcat").1);

        // No SQL/database engine in the closure and no generic table/query API is public.
        let manifest = include_str!("../Cargo.toml");
        let deps: Vec<&str> = manifest
            .split("[dependencies]")
            .nth(1)
            .unwrap()
            .lines()
            .skip(1)
            .take_while(|l| !l.starts_with('['))
            .filter_map(|l| l.split('=').next().map(str::trim))
            .filter(|n| !n.is_empty())
            .collect();
        for dep in &deps {
            assert!(["hsk-studio-accord", "hsk-studio-folio", "hsk-studio-observe", "sha2"].contains(dep), "unexpected dependency {dep}");
        }
        let sources = [
            include_str!("../src/lib.rs"),
            include_str!("../src/extract.rs"),
            include_str!("../src/catalog_bytes.rs"),
            include_str!("../src/wal_snapshot.rs"),
            include_str!("../src/source.rs"),
            include_str!("../src/error.rs"),
            include_str!("../src/library.rs"),
            include_str!("../src/lua_rules.rs"),
            include_str!("../src/xmp.rs"),
            manifest,
        ];
        for needle in ["rusqlite", "libsqlite3", "sqlx", "turso", "limbo", "libsql", "sea-orm", "diesel", "surrealdb", "sqlite3_"] {
            assert!(sources.iter().all(|s| !s.contains(needle)), "forbidden engine reference {needle}");
        }
        let lib = sources[0];
        assert!(!lib.contains("pub mod"), "internal modules stay private");
        assert_eq!(
            PUBLIC_ENTRYPOINTS,
            [
                "extract_catalog",
                "extract_library",
                "parse_smart_rules",
                "resolve_hierarchy",
                "decode_xmp_packet",
                "report"
            ]
        );
        for name in PUBLIC_ENTRYPOINTS {
            assert!(DESCRIPTOR.contains(name));
        }
    });
}

#[test]
fn report_and_descriptor() {
    bounded_case("report_and_descriptor", || {
        #[derive(Default)]
        struct VecSink(Vec<(DeliveryClass, Vec<u8>)>);
        impl SinkPort for VecSink {
            fn try_send(&mut self, class: DeliveryClass, bytes: &[u8]) -> Result<(), DeliveryError> {
                self.0.push((class, bytes.to_vec()));
                Ok(())
            }
        }
        let observe = || {
            let actor = ActorContext::new("account", "principal", "owner", "owner-principal", "space", "session").unwrap();
            let resource = DomainId::parse("SDOC-019abcde-0000-7000-8000-000000000001").unwrap();
            Observe::new(7, 1, resource, actor, Budget::new(1, 2048).unwrap())
        };
        let live = CancellationToken::default();
        let dead = CancellationToken::default();
        dead.cancel();
        let ok = extract(&grant_main(&row_image()));
        let ungranted = extract(&Grant { main: Box::new(SliceSource::new(row_image())), wal: WalGrant::PresentButNotGranted });
        let canceled = extract_catalog(&grant_main(&row_image()), &Limits::default(), &dead);
        let corrupt = extract(&grant_main(&image(vec![interior_page(&[3], 3), interior_page(&[2], 2)])));
        let cases = [
            (ok, &live, Outcome::Success),
            (ungranted, &live, Outcome::Failure(FailureCode::Unavailable)),
            (canceled, &dead, Outcome::Canceled),
            (corrupt, &live, Outcome::Failure(FailureCode::Validation)),
        ];
        for (result, token, want) in cases {
            let (mut obs, mut sink) = (observe(), VecSink::default());
            let receipt = report(&result, 7, 1, token, &mut obs, &mut sink).expect("one terminal frame");
            assert_eq!(receipt.outcome, want);
            assert_eq!(sink.0.len(), 1, "exactly one frame");
            assert_eq!(sink.0[0].0, DeliveryClass::Terminal);
            assert!(!sink.0[0].1.windows(5).any(|w| w == b"Adobe"), "no catalog content in telemetry");
        }
        // Success with losses is still Success; the descriptor says what the API is not.
        assert!(!extract(&grant_main(&row_image())).unwrap().is_lossless());
        for must in ["fixed extraction only", "no SQL engine", "not full conversion", "ungranted_wal", "source_changed"] {
            assert!(DESCRIPTOR.contains(must), "descriptor mentions {must}");
        }
        assert_eq!(DESCRIPTOR.matches('{').count(), DESCRIPTOR.matches('}').count());
        assert_eq!(DESCRIPTOR.matches('[').count(), DESCRIPTOR.matches(']').count());
    });
}

// ---- R5 remediation regressions (hostile images built by hand) ----

/// Interior page 1 (database header + `sqlite_master` split over leaf pages 2 and 3) naming one
/// table per entry with its root at page 4.., then one leaf page per table, then `extra` pages.
fn tables_image(tables: &[(&str, &str, Vec<Vec<u8>>)], extra: Vec<Vec<u8>>) -> Vec<u8> {
    let master: Vec<Vec<u8>> = tables
        .iter()
        .enumerate()
        .map(|(i, (name, sql, _))| {
            let rec = record(&[V::Text("table"), V::Text(name), V::Text(name), V::Int(4 + i as i64), V::Text(sql)]);
            leaf_cell(i as u64 + 1, &rec)
        })
        .collect();
    let (left, right) = master.split_at(master.len() / 2);
    let mut link = 2u32.to_be_bytes().to_vec();
    link.extend(varint(left.len() as u64));
    let mut page1 = pack(0x05, 100, 12, &[link], 3);
    let mut pages = vec![leaf_page(0, left), leaf_page(0, right)];
    pages.extend(tables.iter().map(|(_, _, cells)| leaf_page(0, cells)));
    pages.extend(extra);
    page1[..16].copy_from_slice(b"SQLite format 3\0");
    page1[16..18].copy_from_slice(&(PS as u16).to_be_bytes());
    page1[18] = 1;
    page1[19] = 1;
    page1[21..24].copy_from_slice(&[64, 32, 32]);
    page1[24..28].copy_from_slice(&1u32.to_be_bytes());
    page1[28..32].copy_from_slice(&(pages.len() as u32 + 1).to_be_bytes());
    page1[56..60].copy_from_slice(&1u32.to_be_bytes());
    page1[92..96].copy_from_slice(&1u32.to_be_bytes());
    let mut out = page1;
    for p in pages {
        out.extend(p);
    }
    out
}

const IMAGES_SQL: &str = "CREATE TABLE Adobe_images(id_global,rootFile,rating)";

/// `rows` leaf cells that all carry the record `rec` (rowids 1..=rows) and spill into ONE shared
/// overflow chain starting at page 3, plus that chain's pages.
fn shared_spill(rec: &[u8], rows: u64) -> (Vec<u8>, Vec<Vec<u8>>) {
    let total = rec.len() as u64;
    let (u, x) = (PS as u64, PS as u64 - 35);
    assert!(total > x, "record must spill");
    let m = (u - 12) * 32 / 255 - 23;
    let k = m + (total - m) % (u - 4);
    let local = if k <= x { k } else { m } as usize;
    let cells: Vec<Vec<u8>> = (1..=rows)
        .map(|rowid| {
            let mut cell = varint(total);
            cell.extend(varint(rowid));
            cell.extend(&rec[..local]);
            cell.extend(3u32.to_be_bytes());
            cell
        })
        .collect();
    let chunks: Vec<&[u8]> = rec[local..].chunks(PS - 4).collect();
    let chain = chunks
        .iter()
        .enumerate()
        .map(|(i, chunk)| {
            let next = if i + 1 < chunks.len() { 4 + i as u32 } else { 0 };
            let mut page = overflow_page(next);
            page[4..4 + chunk.len()].copy_from_slice(chunk);
            page
        })
        .collect();
    (leaf_page(0, &cells), chain)
}

#[test]
fn shared_overflow_chain_across_rows_is_refused() {
    bounded_case("shared_overflow_chain_across_rows_is_refused", || {
        let big = "x".repeat(499);
        let rec = record(&[V::Null, V::Text(&big), V::Int(7), V::Int(3)]);
        let build = |rows: u64| {
            let (leaf, chain) = shared_spill(&rec, rows);
            let mut pages = vec![leaf];
            pages.extend(chain);
            image(pages)
        };
        // A legitimate row owns its overflow chain: it decodes through it.
        let one = extract_with(&build(1), Limits::default()).expect("one row owns its chain");
        assert_eq!(one.photos.len(), 1);
        assert_eq!(one.photos[0].global_id.as_deref(), Some(big.as_str()));
        // Six rows pointing at the same overflow pages would retain six copies of one stored
        // value (the amplification behind the reviewed out-of-memory abort): refused, typed.
        assert_eq!(
            extract_with(&build(6), Limits::default()).unwrap_err(),
            LrcatError::Cycle("overflow_shared")
        );
    });
}

/// Entries the proposal itself holds for the entities that have a proposal vector.
fn produced(p: &CatalogProposal, entity: &str) -> Option<u64> {
    Some(match entity {
        "Adobe_images" => p.photos.len(),
        "Adobe_imageDevelopSettings" => p.develop.len(),
        "AgLibraryCollection" => p.collections.len(),
        "AgLibraryCollectionImage" => p.collection_members.len(),
        "AgLibraryKeyword" => p.keywords.len(),
        "AgLibraryKeywordImage" => p.keyword_assignments.len(),
        "Adobe_variablesTable" => usize::from(p.schema_version.is_some()),
        _ => return None,
    } as u64)
}

/// Whole-row loss occurrences the proposal lists for an entity (field-level notes excluded).
fn noted_lost(p: &CatalogProposal, entity: &str) -> u64 {
    const WHOLE_ROW: [&str; 9] = [
        "orphan_row",
        "duplicate_rowid",
        "variables_not_mapped",
        "xmp_blob_not_inflated",
        "exif_harvest_not_mapped",
        "develop_history_not_mapped",
        "develop_snapshot_not_mapped",
        "collection_content_not_decoded",
        "unmapped_table",
    ];
    p.losses
        .iter()
        .filter(|l| WHOLE_ROW.contains(&l.code))
        .filter(|l| {
            l.entity == entity
                || (l.entity == "unmapped_table" && entity.strip_prefix("table:") == Some(l.field.as_str()))
        })
        .map(|l| l.count)
        .sum()
}

/// Three independent sources must agree: the walker's row count, the proposal content and the
/// loss notes.
fn assert_independent_reconciliation(p: &CatalogProposal) {
    assert!(!p.counts.is_empty());
    for c in &p.counts {
        assert_eq!(c.rows_seen, c.mapped + c.lost, "seen == mapped + lost for {}", c.entity);
        if let Some(n) = produced(p, &c.entity) {
            assert_eq!(c.mapped, n, "mapped of {} is the number of entries in the proposal", c.entity);
        }
        assert_eq!(c.lost, noted_lost(p, &c.entity), "lost of {} equals its whole-row loss notes", c.entity);
    }
}

#[test]
fn reconciliation_is_cross_checked_not_constructed() {
    bounded_case("reconciliation_is_cross_checked_not_constructed", || {
        for p in [extract(&grant_main(MAIN)).unwrap(), extract(&grant_wal(WAL_MAIN, WAL)).unwrap()] {
            assert_independent_reconciliation(&p);
            assert!(p.reconciles());
            // The claim can fail: a proposal that lost an entry, lost a count or carries a count
            // that disagrees with its content no longer reconciles.
            let mut fewer = p.clone();
            fewer.photos.pop();
            assert!(!fewer.reconciles(), "mapped must equal the entries actually present");
            let mut keywords = p.clone();
            keywords.keyword_assignments.clear();
            assert!(!keywords.reconciles(), "emptied assignments no longer match their count");
            let mut skewed = p.clone();
            skewed.counts.iter_mut().find(|c| c.entity == "Adobe_images").unwrap().lost += 1;
            assert!(!skewed.reconciles());
            let mut missing = p.clone();
            missing.counts.retain(|c| c.entity != "Adobe_images");
            assert!(!missing.reconciles(), "the required table always has a count");
        }

        // A repeated rowid (corrupt b-tree) used to be counted as two mapped rows although the
        // second silently replaced the first; now the later row is a typed whole-row loss.
        let root_sql = "CREATE TABLE AgLibraryRootFolder(absolutePath)";
        let dup = tables_image(
            &[
                (
                    "AgLibraryRootFolder",
                    root_sql,
                    vec![leaf_cell(5, &record(&[V::Text("/a")])), leaf_cell(5, &record(&[V::Text("/b")]))],
                ),
                ("Adobe_images", IMAGES_SQL, vec![]),
            ],
            vec![],
        );
        let p = extract_with(&dup, Limits::default()).expect("duplicate rowid is a loss, not a failure");
        let c = p.count("AgLibraryRootFolder").unwrap();
        assert_eq!((c.rows_seen, c.mapped, c.lost), (2, 1, 1));
        assert_eq!(p.loss("AgLibraryRootFolder", "duplicate_rowid").unwrap().count, 1);
        assert_independent_reconciliation(&p);
        assert!(p.reconciles());
    });
}

#[test]
fn retained_text_budget_counts_every_copy() {
    bounded_case("retained_text_budget_counts_every_copy", || {
        // One stored 440-byte root path, one folder, one file; every photo row resolves its path
        // through them and its proposal gets its own copy of the root path (and folder/file text).
        let path = "p".repeat(440);
        let build_with = |photos: u64, root: &str| {
            let rows: Vec<Vec<u8>> = (1..=photos)
                .map(|id| leaf_cell(id, &record(&[V::Text("g"), V::Int(1), V::Int(3)])))
                .collect();
            tables_image(
                &[
                    ("AgLibraryRootFolder", "CREATE TABLE AgLibraryRootFolder(absolutePath)", vec![leaf_cell(1, &record(&[V::Text(root)]))]),
                    ("AgLibraryFolder", "CREATE TABLE AgLibraryFolder(rootFolder,pathFromRoot)", vec![leaf_cell(1, &record(&[V::Int(1), V::Text("a")]))]),
                    (
                        "AgLibraryFile",
                        "CREATE TABLE AgLibraryFile(folder,idx_filename,baseName,extension)",
                        vec![leaf_cell(1, &record(&[V::Int(1), V::Text("x.cr2"), V::Null, V::Null]))],
                    ),
                    ("Adobe_images", IMAGES_SQL, rows),
                ],
                vec![],
            )
        };
        let build = |photos: u64| build_with(photos, &path);
        let tight = Limits { max_retained_text_bytes: 9_000, ..Limits::default() };

        // Within the default budget the amplification is real: 16 photos, 16 copies of the path.
        let many = extract_with(&build(16), Limits::default()).expect("default budget admits the small catalog");
        let copies: usize = many.photos.iter().map(|p| p.root_path.as_deref().map_or(0, str::len)).sum();
        assert_eq!(copies, 16 * path.len());
        assert!(copies as u64 > tight.max_retained_text_bytes / 2 && many.reconciles());
        // A single photo fits the tight budget; the same stored path referenced by 16 rows does
        // not, because every copy is charged: typed limit, no proposal.
        assert_eq!(extract_with(&build(1), tight).expect("one copy fits").photos.len(), 1);
        // Same 16 rows and the same budget with a 1-byte root path fit: only the copied text
        // volume (not the row count) decides, so the cloned text is what is being charged.
        let short = extract_with(&build_with(16, "p"), tight).expect("16 rows of tiny text fit");
        assert_eq!(short.photos.len(), 16);
        assert_eq!(
            extract_with(&build(16), tight).unwrap_err(),
            LrcatError::Limit(LimitKind::RetainedText)
        );
        // The budget bounds the schema too: sqlite_master table names are only bounded by the record.
        let bare = Limits { max_retained_text_bytes: 300, ..Limits::default() };
        assert_eq!(extract_with(&build(1), bare).unwrap_err(), LrcatError::Limit(LimitKind::RetainedText));
        // The budget itself cannot be disabled by configuration.
        let off = Limits { max_retained_text_bytes: u64::MAX, ..Limits::default() };
        assert_eq!(extract_with(&build(1), off).unwrap_err(), LrcatError::Limit(LimitKind::Configuration));
    });
}

fn patched(mut bytes: Vec<u8>, at: usize, with: &[u8]) -> Vec<u8> {
    bytes[at..at + with.len()].copy_from_slice(with);
    bytes
}

#[test]
fn sizes_at_numeric_maximum_are_typed_refusals() {
    bounded_case("sizes_at_numeric_maximum_are_typed_refusals", || {
        let refused = |bytes: &[u8], limits: Limits| extract_with(bytes, limits).unwrap_err();
        let d = Limits::default;
        // Header: page size 0 and 65535 (not a power of two), page count u32::MAX.
        assert_eq!(refused(&patched(row_image(), 16, &[0, 0]), d()), LrcatError::Corrupt("page_size"));
        assert_eq!(refused(&patched(row_image(), 16, &[0xff, 0xff]), d()), LrcatError::Corrupt("page_size"));
        assert_eq!(refused(&patched(row_image(), 28, &[0xff; 4]), d()), LrcatError::Limit(LimitKind::Pages));
        // Table root page u32::MAX and a root that does not fit u32 (negative rowid-like value).
        assert_eq!(refused(&image_rooted("Adobe_images", i64::from(u32::MAX), vec![leaf_page(0, &[])]), d()), LrcatError::Corrupt("page_number"));
        assert_eq!(refused(&image_rooted("Adobe_images", i64::MIN, vec![leaf_page(0, &[])]), d()), LrcatError::Unsupported("virtual_table"));
        // b-tree page: cell count 0xffff, cell pointer 0xffff, interior child u32::MAX.
        assert_eq!(refused(&patched(row_image(), PS + 3, &[0xff, 0xff]), d()), LrcatError::Corrupt("cell_pointer_array"));
        assert_eq!(refused(&patched(row_image(), PS + 8, &[0xff, 0xff]), d()), LrcatError::Corrupt("cell_bounds"));
        let child_max = image(vec![interior_page(&[u32::MAX], 3), leaf_page(0, &[])]);
        assert_eq!(refused(&child_max, d()), LrcatError::Corrupt("child_page"));
        // Cell: declared payload u64::MAX (nine 0xff varint bytes), overflow pointer u32::MAX.
        let mut payload_max = vec![0xff; 9];
        payload_max.extend(varint(1));
        assert_eq!(refused(&image(vec![leaf_page(0, &[payload_max])]), d()), LrcatError::Limit(LimitKind::Payload));
        let ptr_max = image(vec![leaf_page(0, &[spilling_cell_to(2000, u32::MAX)])]);
        assert_eq!(refused(&ptr_max, d()), LrcatError::Corrupt("overflow_pointer"));
        // A 1 GiB declared payload in a 2 KiB file with every cap raised to its widest: refused
        // typed after reading the (missing) chain, without reserving the declared size.
        let wide = Limits { max_payload: 1 << 30, max_overflow_pages: u32::MAX, ..d() };
        let huge = image(vec![leaf_page(0, &[spilling_cell_to(1 << 30, 3)]), overflow_page(0)]);
        assert_eq!(refused(&huge, wide), LrcatError::Corrupt("overflow_chain_short"));
        // Record header: header size claims more than the payload; serial type u64::MAX.
        let mut bad_header = varint(200);
        bad_header.extend(vec![0u8; 9]);
        assert_eq!(refused(&image(vec![leaf_page(0, &[leaf_cell(1, &bad_header)])]), d()), LrcatError::Corrupt("record_header"));
        let mut serial_max = vec![10u8];
        serial_max.extend(vec![0xff; 9]);
        assert_eq!(refused(&image(vec![leaf_page(0, &[leaf_cell(1, &serial_max)])]), d()), LrcatError::Corrupt("record_truncated"));
    });
}
