//! Lane SMB-TYPE-SENTINELS-REGISTRY-1, item 2: STU-TYP-170 feature registry (v02.228, SD-30).
//!
//! The expected rows are the Master Spec v02.229 STU-TYP-170 table, typed in below; counts and the
//! two derived lists come from the clause text ("245 tags", the 16 registered tags the 229-tag
//! union omits, `abrv` and `just` observed-only). Nothing is read from this crate's own table.
#![allow(dead_code)]
include!("support/mod.rs");

fn tag(s: &str) -> FeatureTag {
    FeatureTag::parse(s).unwrap()
}

/// The 16 registered tags the 229-tag union omits (clause text).
const REGISTERED_NOT_OBSERVED: [&str; 16] = [
    "abvf", "apkn", "c2sc", "chws", "frac", "jalt", "lnum", "mset", "onum", "pnum", "smcp", "subs",
    "sups", "tnum", "vapk", "vchw",
];

#[test]
fn mt_30955_registry_holds_the_245_tag_table() {
    let reg = registry();
    assert_eq!(reg.len(), 245);
    assert!(reg.windows(2).all(|w| w[0].tag < w[1].tag), "sorted, unique");
    for t in REGISTERED_NOT_OBSERVED.iter().chain(["abrv", "just"].iter()) {
        assert!(registry_entry(tag(t)).is_some(), "{t} is a row of the table");
    }
    // Ranges count every member: cv01-cv99 and ss01-ss20.
    for n in 1..=99 {
        assert!(registry_entry(tag(&format!("cv{n:02}"))).is_some(), "cv{n:02}");
    }
    for n in 1..=20 {
        assert!(registry_entry(tag(&format!("ss{n:02}"))).is_some(), "ss{n:02}");
    }
    // The registry is not closed: tags outside it are still stored and applied.
    for t in ["xyz1", "ss21", "cv00", "    "] {
        assert!(registry_entry(tag(t)).is_none(), "{t}");
        let mut map = FeatureMap::new();
        map.set(tag(t), FeatureValue::Binary(true)).unwrap();
        assert_eq!(map.len(), 1);
    }
}
