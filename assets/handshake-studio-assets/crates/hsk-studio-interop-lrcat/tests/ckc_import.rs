//! Lane SMB-CKC-PORTS: develop-settings mapping, zlib XMP inflate through the shared DEFLATE, and
//! the library proposal imported end to end through the CKC port contract, proven against the
//! kernel-bindings TEST-ONLY double (feature ckc-test-double, dev-dependency only). Synthetic
//! fixtures; Lightroom parity UNVERIFIED (D5).
use hsk_studio_accord::CancellationToken;
use hsk_studio_interop_lrcat::{
    Disposition, Grant, LibraryLimits, LibraryProposal, Limits, SliceSource, WhiteBalanceSource, XmpRating,
    apply_ckc_import, derived_uuid_bytes, extract_library, plan_ckc_import,
};
use hsk_studio_kernel_bindings::{
    ActorKind, AssetFacts, AssetId, CatalogField, CatalogOp, CatalogRule, CatalogValue, CkcActor, CkcCatalogPort,
    CkcLimits, CkcTestDouble, CollectionId, ColorLabel, Criterion, DoubleCollection, Flag, KeywordPath, Rating,
    SearchRequest,
};
use sha2::{Digest, Sha256};

const MAIN: &[u8] = include_bytes!("fixtures/synthetic-main.lrcat");
const LIBRARY: &[u8] = include_bytes!("fixtures/synthetic-library.lrcat");
const LIBRARY_2: &[u8] = include_bytes!("fixtures/synthetic-library-2.lrcat");
const PROVENANCE_2: &str = include_str!("fixtures/PROVENANCE-library-2.json");

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
    match rx.recv_timeout(Duration::from_secs(60)) {
        Ok(Ok(())) => {}
        Ok(Err(p)) => resume_unwind(p),
        Err(e) => panic!("case {name} exceeded/lost declared 60s deadline: {e}"),
    }
}

fn library(bytes: &[u8]) -> LibraryProposal {
    let grant = Grant::main_only(Box::new(SliceSource::new(bytes.to_vec())));
    extract_library(&grant, &Limits::default(), &LibraryLimits::default(), &CancellationToken::default()).unwrap()
}

fn sum(p: &LibraryProposal, entity: &str, code: &str) -> u64 {
    p.losses.iter().filter(|l| l.entity == entity && l.code == code).map(|l| l.count).sum()
}

#[test]
fn develop_settings_and_zlib_xmp_map_typed() {
    bounded_case("develop_zlib", || {
        let sha: String = Sha256::digest(LIBRARY_2).iter().map(|b| format!("{b:02x}")).collect();
        assert!(PROVENANCE_2.contains(&sha), "fixture bytes match PROVENANCE-library-2.json");
        let p = library(LIBRARY_2);
        let d = &p.develop;
        assert_eq!(d.len(), 4);
        assert_eq!(d[0].exposure_stops, Some(0.5));
        let t = d[0].tone;
        assert_eq!((t.contrast, t.highlights, t.shadows, t.whites, t.blacks), (25.0, -40.0, 30.0, 10.0, -5.0));
        let pr = d[0].presence;
        assert_eq!((pr.vibrance, pr.saturation, pr.clarity), (15.0, -10.0, 20.0));
        assert!(matches!(d[0].white_balance, WhiteBalanceSource::Absolute(w) if w.kelvin == 5500.0 && w.tint == 10.0));
        assert_eq!(d[0].mapped_keys, 12);
        // Out of range values are refused, never clipped; As Shot stays as shot.
        assert_eq!((d[1].exposure_stops, d[1].tone.contrast, d[1].white_balance), (None, 0.0, WhiteBalanceSource::AsShot));
        assert_eq!(d[2].white_balance, WhiteBalanceSource::Unmapped);
        assert_eq!(d[3].white_balance, WhiteBalanceSource::Unmapped);
        let e = "Adobe_imageDevelopSettings";
        assert_eq!(sum(&p, e, "develop_value_out_of_range"), 3, "Exposure2012 7, Contrast2012 150, Temperature 99999");
        assert_eq!(sum(&p, e, "develop_value_not_number"), 1);
        assert_eq!(sum(&p, e, "develop_white_balance_invalid"), 1);
        assert_eq!(sum(&p, e, "develop_white_balance_preset_unmapped"), 1);
        assert_eq!(sum(&p, e, "develop_key_unmapped"), 1, "Sharpness");
        let scale = p.loss(e, "develop_scale_unverified").unwrap();
        assert_eq!((scale.count, scale.disposition), (2, Disposition::Approximate));

        // zlib XMP: 4-byte-length-prefixed and bare streams inflate; bad Adler-32, wrong declared
        // length and an 8 MiB inflation bomb (cap 4 MiB) are refused per row.
        for i in [0, 1] {
            let x = p.photos[i].xmp.as_ref().expect("inflated packet");
            assert_eq!((x.rating, x.label.as_deref(), x.keywords.as_slice()), (Some(XmpRating::Stars(4)), Some("Green"), &["zlib".to_owned()][..]));
        }
        let m = "Adobe_AdditionalMetadata";
        assert_eq!(sum(&p, m, "xmp_zlib_checksum"), 1);
        assert_eq!(sum(&p, m, "xmp_inflate_length_mismatch"), 1);
        assert_eq!(sum(&p, m, "xmp_inflate_over_limit"), 1);
        let c = p.count(m).unwrap();
        assert_eq!((c.rows_seen, c.mapped, c.lost), (5, 2, 3));
    });
}

/// Test resolver: lrcat image n is CKC asset 00000000-0000-0000-0000-{n:012x}.
fn asset(n: i64) -> AssetId {
    AssetId::new(&format!("00000000-0000-0000-0000-{n:012x}")).unwrap()
}

fn name(a: &str) -> i64 {
    i64::from_str_radix(&a[24..], 16).unwrap()
}

fn op() -> CkcActor {
    CkcActor { kind: ActorKind::Operator, principal_id: "importer".into() }
}

fn import(bytes: &[u8]) -> (LibraryProposal, CkcTestDouble, hsk_studio_interop_lrcat::CkcImportPlan) {
    let l = CkcLimits::default();
    let p = library(bytes);
    let double = CkcTestDouble::new(l);
    for photo in &p.catalog.photos {
        let facts = AssetFacts { original_filename: photo.file_name.clone().unwrap_or_default(), ..AssetFacts::default() };
        double.seed_asset(asset(photo.local_id), facts).unwrap();
    }
    let resolve = |id: i64| Some(asset(id));
    let plan = plan_ckc_import(&p, &resolve, &l).unwrap();
    (p, double, plan)
}

fn hits(d: &CkcTestDouble, rule: CatalogRule) -> Vec<i64> {
    let req = SearchRequest { rule: Some(rule), sort: vec![], offset: 0, limit: 1000 };
    d.search(&op(), &req).unwrap().assets.iter().map(|a| name(a.asset_id.as_str())).collect()
}

fn m(field: CatalogField, op: CatalogOp, value: CatalogValue) -> CatalogRule {
    CatalogRule::Match(Criterion { field, op, value, value2: None })
}

#[test]
fn library_proposal_imports_through_ckc_port_double() {
    bounded_case("ckc_import", || {
        let l = CkcLimits::default();
        // Library fixture: sets, smart rule translation, dropped parents, flags, labels, synonyms.
        let (p, d, plan) = import(LIBRARY);
        let receipts = apply_ckc_import(&d, &op(), &plan).unwrap();
        let fp = p.catalog.fingerprint.sha256_main;
        let cid = |n: i64| CollectionId::from_bytes(derived_uuid_bytes(&[b"lrcat-collection", &fp, &n.to_be_bytes()]));
        assert_eq!(&cid(3).as_str()[14..15], "8", "name-based UUID version 8");
        let expected_rule = CatalogRule::All(vec![
            m(CatalogField::Rating, CatalogOp::Ge, CatalogValue::Int(4)),
            CatalogRule::Any(vec![
                m(CatalogField::Keywords, CatalogOp::ContainsAny, CatalogValue::Text("rome italy".into())),
                m(CatalogField::ColorLabel, CatalogOp::Eq, CatalogValue::ColorLabel(ColorLabel::Red)),
            ]),
        ]);
        assert!(matches!(d.collection_state(&cid(3)), Some(DoubleCollection::Smart { rule, parent: Some(par), .. }) if rule == expected_rule && par == cid(1)));
        assert!(matches!(d.collection_state(&cid(7)), Some(DoubleCollection::Plain { parent: None, .. })), "non-set parent dropped");
        assert!(d.collection_state(&cid(4)).is_none(), "refused rule bomb: smart collection not created");
        let lp = |code: &str| plan.losses.iter().find(|x| x.code == code).map(|x| x.count);
        assert_eq!(lp("collection_parent_dropped"), Some(1));
        assert_eq!(lp("smart_rule_not_translatable"), Some(1));
        assert_eq!(lp("label_not_in_ckc_set"), Some(1));
        assert_eq!(hits(&d, m(CatalogField::Rating, CatalogOp::Ge, CatalogValue::Int(4))), [4, 7]);
        assert!(hits(&d, expected_rule).is_empty(), "no imported asset has rating >= 4 and a red label or rome/italy keyword");
        let italy = KeywordPath::new(&["Places", "Europe", "Italy"], &l).unwrap();
        assert_eq!(d.synonyms(&italy), ["Italia", "Italie"]);
        let a3 = d.get(&op(), &asset(3)).unwrap();
        assert_eq!((a3.flag, a3.rating, a3.revision), (Flag::Reject, Rating::UNRATED, 2));
        let a1 = d.get(&op(), &asset(1)).unwrap();
        assert_eq!((a1.rating, a1.color_label), (Rating::new(3).unwrap(), Some(ColorLabel::Red)));
        assert_eq!(plan.metadata.len(), 1, "8 photos fit one all-or-nothing batch (one receipt)");
        let events = d.events();
        // Re-applying the same plan replays every receipt; nothing is applied twice.
        assert_eq!(apply_ckc_import(&d, &op(), &plan).unwrap(), receipts);
        assert_eq!(d.get(&op(), &asset(1)).unwrap().revision, 2);
        assert_eq!(d.events(), events, "replays record no second EventLedger event");

        // Main fixture: manual membership and hierarchical keyword assignments.
        let (_, d, plan) = import(MAIN);
        apply_ckc_import(&d, &op(), &plan).unwrap();
        assert_eq!(hits(&d, m(CatalogField::Keywords, CatalogOp::ContainsAny, CatalogValue::Text("italy".into()))), [1, 2]);
        assert_eq!(hits(&d, m(CatalogField::Collections, CatalogOp::Eq, CatalogValue::Text("Favorites".into()))), [1, 2, 3]);
        let a1 = d.get(&op(), &asset(1)).unwrap();
        assert_eq!(a1.collections.len(), 1, "PhotoAsset.collections lists Favorites");
        let travel_italy = KeywordPath::new(&["Travel", "Italy"], &l).unwrap();
        assert!(a1.keywords.contains(&travel_italy), "keyword assigned as its root-first path");
        assert_eq!(plan.losses.iter().find(|x| x.code == "smart_rule_not_translatable").map(|x| x.count), Some(1));
        // A denied principal changes nothing and learns nothing.
        d.deny("intruder").unwrap();
        let intruder = CkcActor { kind: ActorKind::Model, principal_id: "intruder".into() };
        assert!(apply_ckc_import(&d, &intruder, &plan).is_err());
    });
}
