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

/// The 128 rows of the STU-TYP-170 table as the clause writes them: (tag, friendly name, status,
/// "in the 229-tag observed union"). The two range rows carry `*` and `all`.
const SPEC_ROWS: [(&str, &str, &str, &str); 128] = [
    ("aalt", "Access All Alternates", "registered", "yes"),
    ("abvf", "Above-base Forms", "registered", "no"),
    ("abvm", "Above-base Mark Positioning", "registered", "yes"),
    ("abvs", "Above-base Substitutions", "registered", "yes"),
    ("afrc", "Alternative Fractions", "registered", "yes"),
    ("akhn", "Akhand", "registered", "yes"),
    ("apkn", "Kerning for Alternate Proportional Widths", "registered", "no"),
    ("blwf", "Below-base Forms", "registered", "yes"),
    ("blwm", "Below-base Mark Positioning", "registered", "yes"),
    ("blws", "Below-base Substitutions", "registered", "yes"),
    ("calt", "Contextual Alternates", "registered", "yes"),
    ("case", "Case-sensitive Forms", "registered", "yes"),
    ("ccmp", "Glyph Composition / Decomposition", "registered", "yes"),
    ("cfar", "Conjunct Form After Ro", "registered", "yes"),
    ("chws", "Contextual Half-width Spacing", "registered", "no"),
    ("cjct", "Conjunct Forms", "registered", "yes"),
    ("clig", "Contextual Ligatures", "registered", "yes"),
    ("cpct", "Centered CJK Punctuation", "registered", "yes"),
    ("cpsp", "Capital Spacing", "registered", "yes"),
    ("cswh", "Contextual Swash", "registered", "yes"),
    ("curs", "Cursive Positioning", "registered", "yes"),
    ("cv01-cv99", "*", "registered", "all"),
    ("c2pc", "Petite Capitals From Capitals", "registered", "yes"),
    ("c2sc", "Small Capitals From Capitals", "registered", "no"),
    ("dist", "Distances", "registered", "yes"),
    ("dlig", "Discretionary Ligatures", "registered", "yes"),
    ("dnom", "Denominators", "registered", "yes"),
    ("dtls", "Dotless Forms", "registered", "yes"),
    ("expt", "Expert Forms", "registered", "yes"),
    ("falt", "Final Glyph on Line Alternates", "registered", "yes"),
    ("fin2", "Terminal Forms #2", "registered", "yes"),
    ("fin3", "Terminal Forms #3", "registered", "yes"),
    ("fina", "Terminal Forms", "registered", "yes"),
    ("flac", "Flattened Accent Forms", "registered", "yes"),
    ("frac", "Fractions", "registered", "no"),
    ("fwid", "Full Widths", "registered", "yes"),
    ("half", "Half Forms", "registered", "yes"),
    ("haln", "Halant Forms", "registered", "yes"),
    ("halt", "Alternate Half Widths", "registered", "yes"),
    ("hist", "Historical Forms", "registered", "yes"),
    ("hkna", "Horizontal Kana Alternates", "registered", "yes"),
    ("hlig", "Historical Ligatures", "registered", "yes"),
    ("hngl", "Hangul", "deprecated", "yes"),
    ("hojo", "Hojo Kanji Forms (JIS X 0212-1990 Kanji Forms)", "registered", "yes"),
    ("hwid", "Half Widths", "registered", "yes"),
    ("init", "Initial Forms", "registered", "yes"),
    ("isol", "Isolated Forms", "registered", "yes"),
    ("ital", "Italics", "registered", "yes"),
    ("jalt", "Justification Alternates", "registered", "no"),
    ("jp78", "JIS78 Forms", "registered", "yes"),
    ("jp83", "JIS83 Forms", "registered", "yes"),
    ("jp90", "JIS90 Forms", "registered", "yes"),
    ("jp04", "JIS2004 Forms", "registered", "yes"),
    ("kern", "Kerning", "registered", "yes"),
    ("lfbd", "Left Bounds", "registered", "yes"),
    ("liga", "Standard Ligatures", "registered", "yes"),
    ("ljmo", "Leading Jamo Forms", "registered", "yes"),
    ("lnum", "Lining Figures", "registered", "no"),
    ("locl", "Localized Forms", "registered", "yes"),
    ("ltra", "Left-to-right Alternates", "registered", "yes"),
    ("ltrm", "Left-to-right Mirrored Forms", "registered", "yes"),
    ("mark", "Mark Positioning", "registered", "yes"),
    ("med2", "Medial Forms #2", "registered", "yes"),
    ("medi", "Medial Forms", "registered", "yes"),
    ("mgrk", "Mathematical Greek", "registered", "yes"),
    ("mkmk", "Mark to Mark Positioning", "registered", "yes"),
    ("mset", "Mark Positioning via Substitution", "registered", "no"),
    ("nalt", "Alternate Annotation Forms", "registered", "yes"),
    ("nlck", "NLC Kanji Forms", "registered", "yes"),
    ("nukt", "Nukta Forms", "registered", "yes"),
    ("numr", "Numerators", "registered", "yes"),
    ("onum", "Oldstyle Figures", "registered", "no"),
    ("opbd", "Optical Bounds", "deprecated", "yes"),
    ("ordn", "Ordinals", "registered", "yes"),
    ("ornm", "Ornaments", "registered", "yes"),
    ("palt", "Proportional Alternate Widths", "registered", "yes"),
    ("pcap", "Petite Capitals", "registered", "yes"),
    ("pkna", "Proportional Kana", "registered", "yes"),
    ("pnum", "Proportional Figures", "registered", "no"),
    ("pref", "Pre-base Forms", "registered", "yes"),
    ("pres", "Pre-base Substitutions", "registered", "yes"),
    ("pstf", "Post-base Forms", "registered", "yes"),
    ("psts", "Post-base Substitutions", "registered", "yes"),
    ("pwid", "Proportional Widths", "registered", "yes"),
    ("qwid", "Quarter Widths", "registered", "yes"),
    ("rand", "Randomize", "registered", "yes"),
    ("rclt", "Required Contextual Alternates", "registered", "yes"),
    ("rkrf", "Rakar Forms", "registered", "yes"),
    ("rlig", "Required Ligatures", "registered", "yes"),
    ("rphf", "Reph Form", "registered", "yes"),
    ("rtbd", "Right Bounds", "registered", "yes"),
    ("rtla", "Right-to-left Alternates", "registered", "yes"),
    ("rtlm", "Right-to-left Mirrored Forms", "registered", "yes"),
    ("ruby", "Ruby Notation Forms", "registered", "yes"),
    ("rvrn", "Required Variation Alternates", "registered", "yes"),
    ("salt", "Stylistic Alternates", "registered", "yes"),
    ("sinf", "Scientific Inferiors", "registered", "yes"),
    ("size", "Optical size", "registered", "yes"),
    ("smcp", "Small Capitals", "registered", "no"),
    ("smpl", "Simplified Forms", "registered", "yes"),
    ("ss01-ss20", "*", "registered", "all"),
    ("ssty", "Math Script-style Alternates", "registered", "yes"),
    ("stch", "Stretching Glyph Decomposition", "registered", "yes"),
    ("subs", "Subscript", "registered", "no"),
    ("sups", "Superscript", "registered", "no"),
    ("swsh", "Swash", "registered", "yes"),
    ("titl", "Titling", "registered", "yes"),
    ("tjmo", "Trailing Jamo Forms", "registered", "yes"),
    ("tnam", "Traditional Name Forms", "registered", "yes"),
    ("tnum", "Tabular Figures", "registered", "no"),
    ("trad", "Traditional Forms", "registered", "yes"),
    ("twid", "Third Widths", "registered", "yes"),
    ("unic", "Unicase", "registered", "yes"),
    ("valt", "Alternate Vertical Metrics", "registered", "yes"),
    ("vapk", "Kerning for Alternate Proportional Vertical Metrics", "registered", "no"),
    ("vatu", "Vattu Variants", "registered", "yes"),
    ("vchw", "Vertical Contextual Half-width Spacing", "registered", "no"),
    ("vert", "Vertical Alternates", "registered", "yes"),
    ("vhal", "Alternate Vertical Half Metrics", "registered", "yes"),
    ("vjmo", "Vowel Jamo Forms", "registered", "yes"),
    ("vkna", "Vertical Kana Alternates", "registered", "yes"),
    ("vkrn", "Vertical Kerning", "registered", "yes"),
    ("vpal", "Proportional Alternate Vertical Metrics", "registered", "yes"),
    ("vrt2", "Vertical Alternates and Rotation", "registered", "yes"),
    ("vrtr", "Vertical Alternates for Rotation", "registered", "yes"),
    ("zero", "Slashed Zero", "registered", "yes"),
    ("abrv", "(not registered)", "observed_unregistered", "yes"),
    ("just", "(not registered)", "observed_unregistered", "yes"),
];

fn status_of(entry: &RegistryEntry) -> &'static str {
    entry.status.label()
}

#[test]
fn mt_30955_every_table_row_is_in_the_registry_with_name_status_and_observed_flag() {
    assert_eq!(SPEC_ROWS.len(), 128, "126 registry rows plus abrv and just");
    let mut expanded = 0usize;
    for (row_tag, name, status, observed) in SPEC_ROWS {
        let members: Vec<(String, String)> = match row_tag {
            "cv01-cv99" => (1..=99).map(|n| (format!("cv{n:02}"), format!("Character Variant {n}"))).collect(),
            "ss01-ss20" => (1..=20).map(|n| (format!("ss{n:02}"), format!("Stylistic Set {n}"))).collect(),
            _ => vec![(row_tag.to_string(), name.to_string())],
        };
        for (member_tag, member_name) in members {
            let entry = registry_entry(tag(&member_tag)).unwrap_or_else(|| panic!("row {member_tag} is missing"));
            assert_eq!(entry.tag.as_str(), member_tag);
            assert_eq!(entry.name, member_name, "{member_tag} name");
            assert_eq!(status_of(entry), status, "{member_tag} status");
            let want_observed = observed == "yes" || observed == "all";
            assert_eq!(entry.observed, want_observed, "{member_tag} in the observed union");
            expanded += 1;
        }
    }
    // No row is added: the registry holds exactly the expanded table.
    assert_eq!(expanded, 245);
    assert_eq!(registry().len(), 245);
}

#[test]
fn mt_30955_registered_set_is_243_with_the_deprecated_tags_marked() {
    let reg = registry();
    let registered = reg.iter().filter(|e| e.status != FeatureStatus::ObservedUnregistered).count();
    assert_eq!(registered, 243);
    let mut deprecated: Vec<_> =
        reg.iter().filter(|e| e.status == FeatureStatus::Deprecated).map(|e| e.tag.as_str().to_string()).collect();
    deprecated.sort();
    assert_eq!(deprecated, ["hngl", "opbd"]);
    let mut unregistered: Vec<_> = reg
        .iter()
        .filter(|e| e.status == FeatureStatus::ObservedUnregistered)
        .map(|e| e.tag.as_str().to_string())
        .collect();
    unregistered.sort();
    assert_eq!(unregistered, ["abrv", "just"]);
    for t in ["abrv", "just"] {
        assert_eq!(registry_entry(tag(t)).unwrap().name, "(not registered)");
    }
    assert_eq!(FeatureStatus::Registered.label(), "registered");
    assert_eq!(FeatureStatus::Deprecated.label(), "deprecated");
    assert_eq!(FeatureStatus::ObservedUnregistered.label(), "observed_unregistered");
    // The tags OpenType 1.9.1 registers: 126 rows - 2 range rows + 99 + 20 = 243.
    assert_eq!(126 - 2 + 99 + 20, 243);
}

#[test]
fn mt_30955_observed_union_of_229_tags_is_proven_against_the_table() {
    let reg = registry();
    assert_eq!(reg.iter().filter(|e| e.observed).count(), 229, "the captured union is 229 tags");
    // The union omits exactly the 16 registered tags the clause lists, and no others.
    let mut omitted: Vec<_> = reg.iter().filter(|e| !e.observed).map(|e| e.tag.as_str().to_string()).collect();
    omitted.sort();
    let mut expected: Vec<String> = REGISTERED_NOT_OBSERVED.iter().map(|t| t.to_string()).collect();
    expected.sort();
    assert_eq!(omitted, expected);
    assert!(reg.iter().filter(|e| !e.observed).all(|e| e.status == FeatureStatus::Registered));
    // 243 registered - 16 omitted + 2 observed-only = 229; the union carries the deprecated tags.
    assert_eq!(243 - 16 + 2, 229);
    for t in ["hngl", "opbd", "size", "abrv", "just"] {
        assert!(registry_entry(tag(t)).unwrap().observed, "{t}");
    }
    // "At least the 229": every one of them is in the registry, plus the 16 it omits.
    assert!(reg.len() >= 229);
    // 36 tags are new against the 209 tags of the STU-TYP-171 group table.
    assert_eq!(reg.iter().filter(|e| e.groups().next().is_none()).count(), 36);
    assert_eq!(reg.iter().filter(|e| e.groups().next().is_some()).count(), 209);
    for t in ["apkn", "chws", "vapk", "vchw", "sinf", "abrv", "just"] {
        assert_eq!(registry_entry(tag(t)).unwrap().groups().count(), 0, "{t} is in no STU-TYP-171 group");
    }
}

#[test]
fn mt_30955_deprecated_tags_are_stored_and_applied_but_not_offered_for_new_authoring() {
    for entry in registry() {
        let deprecated = matches!(entry.tag.as_str(), "hngl" | "opbd");
        assert_eq!(entry.offered_for_new_authoring(), !deprecated, "{}", entry.tag);
    }
    let mut map = FeatureMap::new();
    for t in ["hngl", "opbd", "abrv", "just"] {
        map.set(tag(t), FeatureValue::Binary(true)).unwrap();
    }
    assert_eq!(map.len(), 4);
    let range = SourceRange { start: 0, end: 4 };
    let features = map.to_features(range);
    assert_eq!(features.len(), 4);
    assert!(features.iter().all(|f| f.value == 1 && f.range == range));
    // The same tag set round-trips through the shaping feature list.
    assert_eq!(FeatureMap::from_features(&features).unwrap(), map);
}

#[test]
fn mt_30959_every_legacy_boolean_tag_is_a_registered_row() {
    for boolean in LegacyBoolean::ALL {
        if let Ok(mapped) = boolean.tag() {
            let entry = registry_entry(mapped).unwrap_or_else(|| panic!("{} maps to an unregistered tag", boolean.name()));
            assert_eq!(entry.status, FeatureStatus::Registered, "{}", boolean.name());
        }
    }
    // The source applications expose `fraction` and `justification_alternate`, yet the 229-tag
    // union omits their tags `frac` and `jalt`; the registry still carries both (clause text).
    for t in ["frac", "jalt"] {
        let entry = registry_entry(tag(t)).unwrap();
        assert!(!entry.observed && entry.status == FeatureStatus::Registered, "{t}");
    }
}
