//! Lane SMB-TYPE-OT-FEATURES-1: OpenType feature surface (STU-TYP-017, STU-TYP-170..175).
//!
//! Independent oracle: every expected font tag, alternate count and glyph id below was read with
//! fontTools 4.62.1 (MIT) from the hash-pinned fixture fonts (GSUB/GPOS FeatureList records
//! reachable from any script/LangSys; largest AlternateSet of type-3 lookups, type-7 resolved;
//! single/ligature substitution targets via `getGlyphID`), never from this crate's output. The
//! oracle script and its output are recorded in the lane notes.
//!
//! MT recipe mapping: `mt_30955_*` STU-TYP-170, `mt_30956_*` STU-TYP-171, `mt_30957_*`
//! STU-TYP-172, `mt_30958_*` STU-TYP-173, `mt_30959_*` STU-TYP-174, `mt_30960_*` STU-TYP-175,
//! `mt_31140_*` STU-TYP-017 offer gating, `mt_31141_*` STU-TYP-017 enumeration.
#![allow(dead_code)]
include!("support/mod.rs");

use serde_json::json;

const INTER: usize = 0;
const ARABIC: usize = 1;

fn tag(s: &str) -> FeatureTag {
    FeatureTag::parse(s).unwrap()
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

struct Shaped {
    glyphs: Vec<u32>,
    receipts: Vec<Feature>,
    states: Vec<([u8; 4], FeatureState)>,
    text: Vec<u8>,
}
impl Shaped {
    fn state(&self, t: &str) -> FeatureState {
        self.states
            .iter()
            .find(|(b, _)| b == t.as_bytes())
            .unwrap_or_else(|| panic!("receipt for {t}"))
            .1
    }
}
/// Shapes `text` with `map` stored on the whole range (`declared` as given).
fn shape(fonts: &Fonts, font: usize, script: &str, text: &str, map: &FeatureMap, declared: bool) -> Shaped {
    let features: Vec<Value> = map
        .iter()
        .map(|(t, v)| json!({"tag": t.as_str(), "value": v.shaping_value(), "declared": declared}))
        .collect();
    let case = json!({
        "case": "opentype_features",
        "text": text,
        "text_sha256": hex(&hash(text.as_bytes())),
        "text_utf8_hex": hex(text.as_bytes()),
        "font_index": font,
        "script": script,
        "language": if script == "Arab" { "ar" } else { "en" },
        "features": features,
    });
    let mut out = None;
    with_case(fonts, &case, None, |request, ports, _, _, _| {
        let result = prepared(request, ports);
        let mut shaped = Shaped {
            glyphs: Vec::new(),
            receipts: Vec::new(),
            states: Vec::new(),
            text: result.request().text_utf8.as_bytes().to_vec(),
        };
        for p in result.paragraphs() {
            for r in p.runs() {
                shaped.glyphs.extend(r.glyphs().iter().map(|g| g.glyph_id));
                for f in r.features() {
                    shaped.receipts.push(f.feature);
                    shaped.states.push((f.feature.tag, f.state));
                }
            }
        }
        out = Some(shaped);
    });
    out.unwrap()
}
fn one(t: &str, v: FeatureValue) -> FeatureMap {
    let mut m = FeatureMap::new();
    m.set(tag(t), v).unwrap();
    m
}
const ON: FeatureValue = FeatureValue::Binary(true);

// ---- availability harness: any font bytes through a caller resolver (no file reads in Type) ----

struct OneFont<'a> {
    bytes: &'a [u8],
    ledger: &'a Ledger,
    handle: u64,
}
impl FontResolver for OneFont<'_> {
    fn revision(&self) -> Result<u64, Error> {
        Ok(1)
    }
    fn resolve(&self, r: &FontResource<'_>) -> Result<FontRead<'_>, Error> {
        if r.identity != "probe" {
            return Err(Error::MissingFont);
        }
        Ok(FontRead {
            bytes: self.bytes,
            resource_revision: 1,
            grant_revision: 1,
            lease: self.ledger.borrow(self.handle, self.bytes.len() as u64)?,
        })
    }
    fn verify_lifetime(&self, _: &FontResource<'_>, read: &FontRead<'_>) -> Result<(), Error> {
        if read.bytes.as_ptr() != self.bytes.as_ptr() {
            return Err(Error::LeaseUnavailable);
        }
        Ok(())
    }
}
struct QueryScene;
impl EpochPort for QueryScene {
    fn current_epoch(&self) -> Result<u64, Error> {
        Ok(1)
    }
}
impl SourceContext for QueryScene {
    fn member(&self, _: &Request<'_>) -> Result<(), Error> {
        Err(Error::UnavailableRead)
    }
    fn read(&self, _: ReadAddress<'_>) -> Result<CurrentRead<'_>, Error> {
        Err(Error::UnavailableRead)
    }
    fn resolved_style(&self, _: ReadAddress<'_>) -> Result<Style<'_>, Error> {
        Err(Error::UnavailableRead)
    }
}
fn query_with(bytes: &[u8], identity: &str, content_hash: [u8; 32], limits: Limits) -> Result<FontFeatures, FeatureError> {
    let ledger = Ledger::new();
    let (handle, lease) = ledger.base(bytes.len() as u64);
    let resolver = OneFont { bytes, ledger: &ledger, handle };
    let scene = QueryScene;
    let input = Input { bytes: 1 };
    let ports = Ports {
        context: &scene,
        resolver: &resolver,
        input_lifetime: &input,
        admission: &ledger,
    };
    let cancel = CancellationToken::default();
    ledger.begin();
    let query = FeatureQuery {
        font: FontResource {
            identity,
            location: "probe",
            content_hash,
            face_index: 0,
            resource_revision: 1,
            grant_revision: 1,
        },
        limits,
        cancellation: &cancel,
        cancel_epoch: 1,
    };
    let result = TextEngine.font_features(query, &ports);
    assert_eq!(
        ledger.snapshot().unwrap().current_requested_bytes,
        bytes.len() as u64,
        "every query owner retired, success or refusal"
    );
    let check = TextEngine.check_opentype_feature(query, &ports, tag("zero"));
    assert_eq!(check.is_ok(), result.is_ok());
    drop(lease);
    result
}
fn query(bytes: &[u8]) -> Result<FontFeatures, FeatureError> {
    query_with(bytes, "probe", hash(bytes), limits())
}
fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(font_root().join(name)).unwrap()
}
fn be16(b: &[u8], at: usize) -> usize {
    usize::from(u16::from_be_bytes([b[at], b[at + 1]]))
}
fn be32(b: &[u8], at: usize) -> usize {
    u32::from_be_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]) as usize
}
fn put16(b: &mut [u8], at: usize, v: u16) {
    b[at..at + 2].copy_from_slice(&v.to_be_bytes());
}
/// sfnt table record offset of `t`.
fn record(font: &[u8], t: &[u8; 4]) -> usize {
    (0..be16(font, 4))
        .map(|i| 12 + 16 * i)
        .find(|&r| &font[r..r + 4] == t)
        .unwrap()
}
/// Absolute GSUB offset and its FeatureList, LookupList and ScriptList absolute offsets.
fn gsub(font: &[u8]) -> (usize, usize, usize, usize) {
    let g = be32(font, record(font, b"GSUB") + 8);
    (g, g + be16(font, g + 6), g + be16(font, g + 8), g + be16(font, g + 4))
}

/// fontTools oracle: (tag, largest alternate set, in GSUB, in GPOS).
const INTER_TAGS: &[(&str, u32)] = &[
    ("aalt", 8), ("calt", 0), ("case", 0), ("ccmp", 0), ("cpsp", 0), ("cv01", 0), ("cv02", 0),
    ("cv03", 0), ("cv04", 0), ("cv05", 0), ("cv06", 0), ("cv07", 0), ("cv08", 0), ("cv09", 0),
    ("cv10", 0), ("cv11", 0), ("dlig", 0), ("dnom", 0), ("frac", 0), ("kern", 0), ("locl", 0),
    ("mark", 0), ("numr", 0), ("ordn", 0), ("pnum", 0), ("salt", 0), ("sinf", 0), ("ss01", 0),
    ("ss02", 0), ("ss03", 0), ("ss04", 0), ("subs", 0), ("sups", 0), ("tnum", 0), ("zero", 0),
];
const INTER_GPOS: &[&str] = &["cpsp", "kern", "mark"];
const ARABIC_TAGS: &[(&str, u32)] = &[
    ("aalt", 4), ("ccmp", 0), ("dlig", 0), ("fina", 0), ("init", 0), ("kern", 0), ("liga", 0),
    ("locl", 0), ("mark", 0), ("medi", 0), ("mkmk", 0), ("pnum", 0), ("rlig", 0), ("rtlm", 0),
    ("tnum", 0),
];
const ARABIC_GPOS: &[&str] = &["kern", "mark", "mkmk"];
const SC_TAGS: &[(&str, u32)] = &[
    ("aalt", 2), ("ccmp", 0), ("dlig", 0), ("fwid", 0), ("halt", 0), ("hist", 0), ("hwid", 0),
    ("kern", 0), ("liga", 0), ("locl", 0), ("mark", 0), ("palt", 0), ("pwid", 0), ("ruby", 0),
    ("vert", 0), ("vhal", 0), ("vkrn", 0), ("vpal", 0), ("vrt2", 0),
];
const SC_GPOS: &[&str] = &["halt", "kern", "mark", "palt", "vert", "vhal", "vkrn", "vpal"];
const SC_BOTH: &[&str] = &["vert"];

fn assert_matches_oracle(features: &FontFeatures, tags: &[(&str, u32)], gpos: &[&str], both: &[&str]) {
    let got: Vec<(String, u32, bool, bool)> = features
        .features()
        .iter()
        .map(|f| (f.tag.as_str().to_string(), f.alternates, f.gsub, f.gpos))
        .collect();
    let want: Vec<(String, u32, bool, bool)> = tags
        .iter()
        .map(|(t, n)| {
            let in_gpos = gpos.contains(t);
            (t.to_string(), *n, !in_gpos || both.contains(t), in_gpos)
        })
        .collect();
    assert_eq!(got, want);
}

// ---------------------------------------------------------------- STU-TYP-170 (MT-30955)

#[test]
fn mt_30955_registry_pins_the_group_table_and_accepts_any_well_formed_tag() {
    let reg = registry();
    // 211 table cells, abvm and blwm listed twice: 209 distinct tags.
    assert_eq!(reg.len(), 209);
    assert!(reg.windows(2).all(|w| w[0].tag < w[1].tag), "sorted, unique");
    let cells: usize = FeatureGroup::ALL.iter().map(|g| g.tags().len()).sum();
    assert_eq!(cells, 211);
    for g in FeatureGroup::ALL {
        for t in g.tags() {
            assert!(registry_entry(t).unwrap().groups().any(|x| x == g), "{t} in {g:?}");
        }
    }
    for t in ["abvm", "blwm"] {
        let groups: Vec<_> = registry_entry(tag(t)).unwrap().groups().collect();
        assert_eq!(groups, vec![FeatureGroup::Indic, FeatureGroup::KerningAndMarks]);
    }
    assert!(registry_entry(tag("ss20")).is_some() && registry_entry(tag("ss21")).is_none());
    assert!(registry_entry(tag("cv99")).is_some() && registry_entry(tag("cv00")).is_none());
    // No closed list: unregistered well-formed tags are stored and emitted for shaping.
    let mut map = FeatureMap::new();
    for t in ["sinf", "xyz1", "ss21", "cv00", "    "] {
        assert!(registry_entry(tag(t)).is_none());
        map.set(tag(t), ON).unwrap();
    }
    let range = SourceRange { start: 0, end: 3 };
    let features = map.to_features(range);
    assert_eq!(features.len(), 5);
    assert!(features.iter().all(|f| !f.declared && f.range == range && f.value == 1));
    // Malformed tags and negative alternate indices refuse without changing the map.
    let before = map.clone();
    for bad in ["liga ", "lig", "", "l\u{e9}g"] {
        assert_eq!(map.set_str(bad, ON), Err(FeatureError::MalformedTag));
    }
    assert_eq!(FeatureTag::new([0x80, b'a', b'b', b'c']), Err(FeatureError::MalformedTag));
    assert_eq!(FeatureValue::alternate_from_signed(-1), Err(FeatureError::NegativeAlternateIndex));
    assert_eq!(
        FeatureValue::alternate_from_signed(i64::from(u32::MAX) + 1),
        Err(FeatureError::AlternateIndexOverflow)
    );
    assert_eq!(FeatureValue::alternate_from_signed(7), Ok(FeatureValue::Alternate(7)));
    assert_eq!(map, before);
    assert_eq!(FeatureError::MalformedTag.code(), "malformed_feature_tag");
}

/// STU-TYP-170 asks for "at least the 229 feature tags observed in the captured design-tool
/// feature union"; that union is not listed in the spec, so the registry carries only the 209
/// tags of the STU-TYP-171 table. MT-30955 AC-001 stays open (spec gap).
#[test]
#[ignore = "NOT_PROVEN: STU-TYP-170 229-tag observed union is not recovered in the spec (MT-30955 AC-001 spec gap)"]
fn mt_30955_observed_union_of_229_tags() {
    assert!(registry().len() >= 229);
}

#[test]
fn mt_30955_registered_and_custom_tags_apply_through_real_gsub() {
    bounded_case("mt_30955_registered_and_custom_tags_apply_through_real_gsub", || {
        let fonts = Fonts::load();
        // `zero` is registered, `sinf` is not; Inter declares both (fontTools: 0 -> uniE06B 1307,
        // ( -> uni208D 1561).
        let mut map = one("zero", ON);
        map.set(tag("sinf"), ON).unwrap();
        for (text, base_glyphs, glyphs) in [("0", [1295], [1307]), ("(", [1340], [1561])] {
            let base = shape(&fonts, INTER, "Latn", text, &FeatureMap::new(), false);
            assert_eq!(base.glyphs, base_glyphs);
            let s = shape(&fonts, INTER, "Latn", text, &map, false);
            assert_eq!(s.glyphs, glyphs);
            assert_eq!(s.state("zero"), FeatureState::Applied);
            assert_eq!(s.state("sinf"), FeatureState::Applied);
        }
        // An alternate index selects that alternate (aalt on 0: [1307, 1313, 1508, ...]).
        let s = shape(&fonts, INTER, "Latn", "0", &one("aalt", FeatureValue::Alternate(3)), false);
        assert_eq!(s.glyphs, vec![1508]);
    });
}

// ---------------------------------------------------------------- STU-TYP-171 (MT-30956)

#[test]
fn mt_30956_group_projection_is_presentation_over_the_tag_map() {
    let labels: Vec<_> = FeatureGroup::ALL.iter().map(|g| g.label()).collect();
    assert_eq!(
        labels,
        [
            "Ligatures", "Alternates", "Stylistic sets", "Character variants", "Figure style",
            "Figure width", "Fractions", "Ordinals", "Vertical position", "Zero", "Caps forms",
            "Positional forms", "Arabic justification", "Indic", "CJK width and form",
            "CJK variants", "Vertical writing", "Kana", "Kerning and marks", "Normalisation"
        ]
    );
    let strs = |g: FeatureGroup| g.tags().iter().map(|t| t.as_str().to_string()).collect::<Vec<_>>();
    assert_eq!(strs(FeatureGroup::Ligatures), ["liga", "dlig", "clig", "rlig", "hlig"]);
    assert_eq!(strs(FeatureGroup::Alternates), ["salt", "calt", "titl", "swsh", "cswh", "ornm", "aalt"]);
    assert_eq!(strs(FeatureGroup::KerningAndMarks), ["kern", "mark", "mkmk", "abvm", "blwm"]);
    assert_eq!(strs(FeatureGroup::Indic).len(), 18);
    assert_eq!(strs(FeatureGroup::StylisticSets).first().unwrap(), "ss01");
    assert_eq!(strs(FeatureGroup::CharacterVariants).last().unwrap(), "cv99");

    let inter = query(&fixture("Inter-Regular.ttf")).unwrap();
    let mut map = FeatureMap::new();
    FeatureGroup::Ligatures.select(&mut map, tag("dlig"), ON).unwrap();
    FeatureGroup::CapsForms.select(&mut map, tag("smcp"), ON).unwrap();
    map.set(tag("xtra"), FeatureValue::Alternate(2)).unwrap();
    // A non-member selection refuses and leaves the map unchanged.
    let before = map.clone();
    assert_eq!(FeatureGroup::Zero.select(&mut map, tag("liga"), ON), Err(FeatureError::TagNotInGroup));
    assert_eq!(map, before);

    let view = project_groups(&map, Some(&inter));
    assert_eq!(map, before, "projection never changes stored tags");
    assert_eq!(view.groups.len(), 20);
    let find = |g: FeatureGroup, t: &str| {
        *view.groups.iter().find(|(x, _)| *x == g).unwrap().1.iter().find(|e| e.tag == tag(t)).unwrap()
    };
    let dlig = find(FeatureGroup::Ligatures, "dlig");
    assert!(dlig.offered() && !dlig.inert() && dlig.stored == Some(ON));
    let smcp = find(FeatureGroup::CapsForms, "smcp");
    assert!(!smcp.offered() && smcp.inert(), "Inter lacks smcp: stored, inert");
    assert_eq!(find(FeatureGroup::Alternates, "aalt").availability, FeatureAvailability::Declared { alternates: 8 });
    // abvm is projected in both of its groups from the one stored entry.
    assert_eq!(find(FeatureGroup::Indic, "abvm"), find(FeatureGroup::KerningAndMarks, "abvm"));
    // Ungrouped: the stored custom tag (inert on Inter) and the font-declared unregistered `sinf`.
    let ungrouped: Vec<_> = view.ungrouped.iter().map(|e| (e.tag.as_str().to_string(), e.offered(), e.inert())).collect();
    assert_eq!(ungrouped, [("sinf".to_string(), true, false), ("xtra".to_string(), false, true)]);
    // Selections read back from the map.
    assert_eq!(FeatureGroup::Ligatures.selection(&map), vec![(tag("dlig"), ON)]);
    // Without a font, availability is unknown, never assumed.
    let view = project_groups(&map, None);
    assert!(view.groups.iter().flat_map(|(_, e)| e).all(|e| e.availability == FeatureAvailability::Unknown));
}

// ---------------------------------------------------------------- STU-TYP-172 (MT-30957)

#[test]
fn mt_30957_inert_feature_round_trips_and_text_bytes_unchanged() {
    bounded_case("mt_30957_inert_feature_round_trips_and_text_bytes_unchanged", || {
        let fonts = Fonts::load();
        let text = "0 office";
        let mut map = one("smcp", ON);
        map.set(tag("zero"), ON).unwrap();
        map.set(tag("xyz1"), FeatureValue::Alternate(4)).unwrap();
        let s = shape(&fonts, INTER, "Latn", text, &map, false);
        assert_eq!(s.text, text.as_bytes(), "shaping never rewrites the character stream");
        assert_eq!(s.state("smcp"), FeatureState::UnavailableInert);
        assert_eq!(s.state("xyz1"), FeatureState::UnavailableInert);
        assert_eq!(s.state("zero"), FeatureState::Applied);
        assert_eq!(s.glyphs[0], 1307);
        // The inert entries are reported, not dropped: the receipts rebuild the stored map.
        assert_eq!(FeatureMap::from_features(&s.receipts).unwrap(), map);
        // An inert-only map shapes exactly like no features at all.
        let inert = shape(&fonts, INTER, "Latn", text, &one("smcp", ON), false);
        let none = shape(&fonts, INTER, "Latn", text, &FeatureMap::new(), false);
        assert_eq!(inert.glyphs, none.glyphs);
        // The same stored map on a font that declares other tags (Arabic face) stays stored.
        let arabic = shape(&fonts, ARABIC, "Arab", "\u{0628}\u{0628}", &map, false);
        assert_eq!(FeatureMap::from_features(&arabic.receipts).unwrap(), map);
        assert_eq!(arabic.state("zero"), FeatureState::UnavailableInert);
    });
}

#[test]
fn mt_30957_declared_feature_missing_from_font_refuses_and_conflicts_refuse() {
    bounded_case("mt_30957_declared_feature_missing_from_font_refuses_and_conflicts_refuse", || {
        let fonts = Fonts::load();
        let case = json!({
            "case": "declared", "text": "abc", "text_sha256": hex(&hash(b"abc")),
            "text_utf8_hex": hex(b"abc"), "font_index": INTER, "script": "Latn", "language": "en",
            "features": [{"tag": "smcp", "value": 1, "declared": true}],
        });
        with_case(&fonts, &case, None, |request, ports, _, _, _| {
            reject(request, ports, Error::UnsupportedFeature);
        });
    });
    let range = SourceRange { start: 0, end: 1 };
    let f = |t: &[u8; 4], value: u32, range| Feature { tag: *t, value, range, declared: false };
    assert_eq!(
        FeatureMap::from_features(&[f(b"liga", 1, range), f(b"liga", 0, range)]),
        Err(FeatureError::ConflictingFeatureValue)
    );
    assert_eq!(
        FeatureMap::from_features(&[f(b"liga", 1, range), f(b"dlig", 1, SourceRange { start: 0, end: 2 })]),
        Err(FeatureError::MixedRanges)
    );
    assert_eq!(FeatureMap::from_features(&[f(b"l\x80ga", 1, range)]), Err(FeatureError::MalformedTag));
}

// ---------------------------------------------------------------- STU-TYP-173 (MT-30958)

#[test]
fn mt_30958_availability_matches_independent_reader() {
    bounded_case("mt_30958_availability_matches_independent_reader", || {
        let inter = query(&fixture("Inter-Regular.ttf")).unwrap();
        assert_matches_oracle(&inter, INTER_TAGS, INTER_GPOS, &[]);
        let arabic = query(&fixture("NotoSansArabic-Regular.ttf")).unwrap();
        assert_matches_oracle(&arabic, ARABIC_TAGS, ARABIC_GPOS, &[]);
        // NotoSansSC declares 129 GSUB + 88 GPOS FeatureRecords: over the default limit of 64.
        let sc_bytes = fixture("NotoSansSC-Regular.otf");
        assert_eq!(query(&sc_bytes).unwrap_err(), FeatureError::LayoutOverLimit);
        let mut l = limits();
        l.features = 217;
        let sc = query_with(&sc_bytes, "probe", hash(&sc_bytes), l).unwrap();
        assert_matches_oracle(&sc, SC_TAGS, SC_GPOS, SC_BOTH);
        // Registry membership is never font support.
        assert!(inter.check_opentype_feature(tag("zero")));
        assert!(!inter.check_opentype_feature(tag("liga")));
        assert!(!inter.check_opentype_feature(tag("smcp")));
        assert!(inter.check_opentype_feature(tag("sinf")), "unregistered but declared");
        assert_eq!(inter.alternate_count(tag("aalt")), Some(8));
        assert_eq!(inter.alternate_count(tag("liga")), None);
        assert_eq!(inter.availability(tag("smcp")), FeatureAvailability::NotDeclared);
        assert!(arabic.check_opentype_feature(tag("init")) && !arabic.check_opentype_feature(tag("zero")));
    });
}

#[test]
fn mt_30958_missing_corrupt_and_mismatched_fonts_refuse_explicitly() {
    let bytes = fixture("Inter-Regular.ttf");
    assert_eq!(
        query_with(&bytes, "absent", hash(&bytes), limits()).unwrap_err(),
        FeatureError::Type(Error::MissingFont)
    );
    assert_eq!(
        query_with(&bytes, "probe", [0; 32], limits()).unwrap_err(),
        FeatureError::Type(Error::HashMismatch)
    );
    let junk = vec![0x5a_u8; 4096];
    assert_eq!(query(&junk).unwrap_err(), FeatureError::Type(Error::UnsupportedFont));
    let mut truncated_header = bytes.clone();
    let r = record(&truncated_header, b"GSUB");
    truncated_header[r + 12..r + 16].copy_from_slice(&4u32.to_be_bytes());
    assert_eq!(query(&truncated_header).unwrap_err(), FeatureError::LayoutTableTruncated);
    let mut version = bytes.clone();
    let (g, _, _, _) = gsub(&version);
    put16(&mut version, g, 2);
    assert_eq!(query(&version).unwrap_err(), FeatureError::LayoutVersionUnsupported);
}

#[test]
fn mt_30958_truncated_feature_list_refuses_typed() {
    let mut bytes = fixture("Inter-Regular.ttf");
    let (g, features, _, _) = gsub(&bytes);
    // Cut the GSUB table after the first FeatureRecord (34 are declared).
    let r = record(&bytes, b"GSUB");
    let len = (features - g + 2 + 6) as u32;
    bytes[r + 12..r + 16].copy_from_slice(&len.to_be_bytes());
    assert_eq!(query(&bytes).unwrap_err(), FeatureError::FeatureListTruncated);
}

#[test]
fn mt_30958_out_of_range_lookup_and_feature_indices_refuse_typed() {
    let clean = fixture("Inter-Regular.ttf");
    let (_, features, _, scripts) = gsub(&clean);
    // First FeatureRecord's Feature table, first LookupListIndex -> 0xFFFF (107 lookups).
    let mut bytes = clean.clone();
    let table = features + be16(&bytes, features + 2 + 4);
    assert!(be16(&bytes, table + 2) > 0);
    put16(&mut bytes, table + 4, 0xFFFF);
    assert_eq!(query(&bytes).unwrap_err(), FeatureError::LookupIndexOutOfRange);
    // First script's DefaultLangSys first FeatureIndex -> 0xFFFE (34 features).
    let mut bytes = clean.clone();
    let script = scripts + be16(&bytes, scripts + 2 + 4);
    assert!(be16(&bytes, script) > 0, "DefaultLangSys present");
    let lang = script + be16(&bytes, script);
    assert!(be16(&bytes, lang + 4) > 0);
    put16(&mut bytes, lang + 6, 0xFFFE);
    assert_eq!(query(&bytes).unwrap_err(), FeatureError::FeatureIndexOutOfRange);
}

/// The FeatureList count is the 16-bit maximum and every limit is u64::MAX: the record array is
/// checked against the table length before anything is allocated.
#[test]
fn mt_30958_dims_u32_max_feature_count_refuses_before_allocation() {
    let mut bytes = fixture("Inter-Regular.ttf");
    let (_, features, lookups, _) = gsub(&bytes);
    put16(&mut bytes, features, 0xFFFF);
    let mut l = limits();
    l.features = u64::MAX;
    l.lookups = u64::MAX;
    assert_eq!(query_with(&bytes, "probe", hash(&bytes), l).unwrap_err(), FeatureError::FeatureListTruncated);
    let mut bytes = fixture("Inter-Regular.ttf");
    put16(&mut bytes, lookups, 0xFFFF);
    assert_eq!(query_with(&bytes, "probe", hash(&bytes), l).unwrap_err(), FeatureError::LookupListTruncated);
}

/// Inter: GSUB 34 features / 107 lookups, GPOS 3 / 3 (fontTools). Each table fits the limit
/// alone; the GSUB+GPOS sum does not.
#[test]
fn mt_30958_aggregate_over_budget_refuses() {
    let bytes = fixture("Inter-Regular.ttf");
    let run = |features: u64, lookups: u64| {
        let mut l = limits();
        l.features = features;
        l.lookups = lookups;
        query_with(&bytes, "probe", hash(&bytes), l)
    };
    assert!(run(37, 110).is_ok());
    assert_eq!(run(36, 110).unwrap_err(), FeatureError::LayoutOverLimit);
    assert_eq!(run(37, 109).unwrap_err(), FeatureError::LayoutOverLimit);
    assert_eq!(run(33, 1000).unwrap_err(), FeatureError::LayoutOverLimit);
}

#[derive(Default)]
struct Frames(Vec<Vec<u8>>);
impl obs::SinkPort for Frames {
    fn try_send(&mut self, _: obs::DeliveryClass, bytes: &[u8]) -> Result<(), obs::DeliveryError> {
        self.0.push(bytes.to_vec());
        Ok(())
    }
}

#[test]
fn mt_30958_observe_frames_carry_fixed_codes_only() {
    let actor = ActorContext::new("account", "principal", "owner", "owner-principal", "space", "session").unwrap();
    let cancel = CancellationToken::default();
    let mut sink = Frames::default();
    let mut ok = obs::Observe::new(7, 3, id("STXT"), actor.clone(), obs::Budget::new(0, 0).unwrap());
    let receipt = report_feature_outcome(&mut ok, &mut sink, 7, 3, Ok(()), &cancel).unwrap();
    assert_eq!(receipt.outcome, obs::Outcome::Success);
    let mut failed = obs::Observe::new(8, 3, id("STXT"), actor, obs::Budget::new(0, 0).unwrap());
    let error = FeatureError::LegacyProjectionUnmapped;
    let receipt = report_feature_outcome(&mut failed, &mut sink, 8, 3, Err(&error), &cancel).unwrap();
    assert_eq!(receipt.outcome, obs::Outcome::Failure(obs::FailureCode::Unsupported));
    assert_eq!(feature_failure_code(&FeatureError::StylisticSetBitsLost), obs::FailureCode::Loss);
    assert_eq!(feature_failure_code(&FeatureError::Type(Error::MissingFont)), obs::FailureCode::Unavailable);
    assert_eq!(feature_failure_code(&FeatureError::LookupIndexOutOfRange), obs::FailureCode::Validation);
    for frame in &sink.0 {
        for needle in [&b"hv_kana"[..], b"liga", b"Inter", b"legacy"] {
            assert!(!frame.windows(needle.len()).any(|w| w == needle), "no text in Observe frames");
        }
    }
    assert_eq!(sink.0.len(), 2);
}

// ---------------------------------------------------------------- STU-TYP-174 (MT-30959)

#[test]
fn mt_30959_legacy_booleans_are_tag_map_entries() {
    let mapping: Vec<(&str, Result<String, FeatureError>)> = LegacyBoolean::ALL
        .iter()
        .map(|b| (b.name(), b.tag().map(|t| t.as_str().to_string())))
        .collect();
    let s = |t: &str| Ok(t.to_string());
    assert_eq!(
        mapping,
        vec![
            ("ligatures", s("liga")),
            ("discretionary_ligature", s("dlig")),
            ("contextual_alternate", s("calt")),
            ("swash", s("swsh")),
            ("titling", s("titl")),
            ("ordinal", s("ordn")),
            ("fraction", s("frac")),
            ("slashed_zero", s("zero")),
            ("stylistic_alternate", s("salt")),
            ("overlap_swash", Err(FeatureError::LegacyProjectionUnmapped)),
            ("stretched_alternate", Err(FeatureError::LegacyProjectionUnmapped)),
            ("justification_alternate", s("jalt")),
            ("hv_kana", Err(FeatureError::LegacyProjectionUnmapped)),
            ("proportional_metrics", s("palt")),
        ]
    );
    let mut map = FeatureMap::new();
    for b in LegacyBoolean::ALL {
        if b.tag().is_ok() {
            assert!(!map.legacy_boolean(b).unwrap());
            map.set_legacy_boolean(b, true).unwrap();
            assert!(map.legacy_boolean(b).unwrap());
        } else {
            let before = map.clone();
            assert_eq!(map.set_legacy_boolean(b, true), Err(FeatureError::LegacyProjectionUnmapped));
            assert_eq!(map.legacy_boolean(b), Err(FeatureError::LegacyProjectionUnmapped));
            assert_eq!(map, before);
        }
    }
    assert_eq!(map.len(), 11, "one entry per mapped boolean, no second storage");
    // An explicit false is stored (liga is on by default in shapers).
    map.set_legacy_boolean(LegacyBoolean::Ligatures, false).unwrap();
    assert_eq!(map.get(tag("liga")), Some(FeatureValue::Binary(false)));
    // Tag and boolean writes address the same entry: the last write wins, one authority.
    map.set(tag("zero"), FeatureValue::Alternate(2)).unwrap();
    assert!(map.legacy_boolean(LegacyBoolean::SlashedZero).unwrap());
    map.set(tag("zero"), FeatureValue::Binary(false)).unwrap();
    assert!(!map.legacy_boolean(LegacyBoolean::SlashedZero).unwrap());
}

#[test]
fn mt_30959_legacy_booleans_shape_real_features() {
    bounded_case("mt_30959_legacy_booleans_shape_real_features", || {
        let fonts = Fonts::load();
        let mut map = FeatureMap::new();
        map.set_legacy_boolean(LegacyBoolean::SlashedZero, true).unwrap();
        map.set_legacy_boolean(LegacyBoolean::Ligatures, false).unwrap();
        map.set_legacy_boolean(LegacyBoolean::Ordinal, true).unwrap();
        let s = shape(&fonts, INTER, "Latn", "0", &map, false);
        assert_eq!(s.glyphs, vec![1307]);
        assert_eq!(s.state("zero"), FeatureState::Applied);
        assert_eq!(s.state("liga"), FeatureState::UnavailableInert, "Inter has no liga: stored, inert");
        let back = FeatureMap::from_features(&s.receipts).unwrap();
        assert_eq!(back, map);
        assert!(!back.legacy_boolean(LegacyBoolean::Ligatures).unwrap());
    });
}

// ---------------------------------------------------------------- STU-TYP-175 (MT-30960)

#[test]
fn mt_30960_figure_style_projects_onto_the_tag_pair() {
    let mut map = FeatureMap::new();
    let names: Vec<_> = FigureStyle::ALL.iter().map(|s| s.name()).collect();
    assert_eq!(names, ["default", "tabular_lining", "proportional_oldstyle", "proportional_lining", "tabular_oldstyle"]);
    for style in FigureStyle::ALL {
        map.set_figure_style(style).unwrap();
        assert_eq!(map.figure_style().unwrap(), style);
        let stored: Vec<_> = map.iter().map(|(t, _)| t.as_str().to_string()).collect();
        let want: Vec<String> = match style.tags() {
            None => vec![],
            Some((a, b)) => {
                let mut v = vec![a.as_str().to_string(), b.as_str().to_string()];
                v.sort();
                v
            }
        };
        assert_eq!(stored, want, "exactly the pair, no second storage");
    }
    let mut both = FeatureMap::new();
    both.set(tag("lnum"), ON).unwrap();
    both.set(tag("onum"), ON).unwrap();
    assert_eq!(both.figure_style(), Err(FeatureError::FigureStyleUnrepresentable));
    let partial = one("tnum", ON);
    assert_eq!(partial.figure_style(), Err(FeatureError::FigureStyleUnrepresentable));
    // Explicit off entries read as off.
    let mut off = one("lnum", FeatureValue::Binary(false));
    assert_eq!(off.figure_style(), Ok(FigureStyle::Default));
    off.set_figure_style(FigureStyle::ProportionalOldstyle).unwrap();
    assert_eq!(off.get(tag("lnum")), None);
}

#[test]
fn mt_30960_stylistic_set_bitfield_converts_at_the_import_boundary() {
    let mut map = one("liga", ON);
    for bits in [0u64, 1, 0b101, 1 << 19, (1 << 20) - 1, 0x5_5555] {
        map.import_stylistic_sets(bits).unwrap();
        assert_eq!(u64::from(map.export_stylistic_sets()), bits);
        let sets = FeatureGroup::StylisticSets.selection(&map);
        assert_eq!(sets.len(), bits.count_ones() as usize);
        assert!(sets.iter().all(|(_, v)| *v == ON));
        assert_eq!(map.get(tag("liga")), Some(ON), "other entries untouched");
    }
    map.import_stylistic_sets(0b11).unwrap();
    assert_eq!(map.get(tag("ss01")), Some(ON));
    assert_eq!(map.get(tag("ss02")), Some(ON));
    // Bits beyond ss20 are a typed loss: refused, the map keeps its active sets.
    let before = map.clone();
    for bits in [1u64 << 20, 1 << 63, (1 << 21) | 1] {
        assert_eq!(map.import_stylistic_sets(bits), Err(FeatureError::StylisticSetBitsLost));
        assert_eq!(map, before);
    }
    // Export reads only ss01..ss20 entries that are on.
    map.set(tag("ss03"), FeatureValue::Binary(false)).unwrap();
    assert_eq!(map.export_stylistic_sets(), 0b11);
}

#[test]
fn mt_30960_figure_style_and_sets_shape_real_numerals() {
    bounded_case("mt_30960_figure_style_and_sets_shape_real_numerals", || {
        let fonts = Fonts::load();
        let mut map = FeatureMap::new();
        map.set_figure_style(FigureStyle::TabularLining).unwrap();
        // fontTools: tnum maps comma -> uniE093 1412; Inter declares no lnum.
        let s = shape(&fonts, INTER, "Latn", ",", &map, false);
        assert_eq!(s.glyphs, vec![1412]);
        assert_eq!(s.state("tnum"), FeatureState::Applied);
        assert_eq!(s.state("lnum"), FeatureState::UnavailableInert);
        // ss01 maps 3 -> uniE06D 1309.
        let mut sets = FeatureMap::new();
        sets.import_stylistic_sets(0b1).unwrap();
        let s = shape(&fonts, INTER, "Latn", "3", &sets, false);
        assert_eq!(s.glyphs, vec![1309]);
    });
}

// ---------------------------------------------------------------- STU-TYP-017 (MT-31140, MT-31141)

#[test]
fn mt_31140_features_are_offered_only_when_the_font_declares_them() {
    bounded_case("mt_31140_features_are_offered_only_when_the_font_declares_them", || {
        let inter = query(&fixture("Inter-Regular.ttf")).unwrap();
        let mut map = one("lnum", ON);
        map.set(tag("tnum"), ON).unwrap();
        let view = project_groups(&map, Some(&inter));
        for (group, entries) in &view.groups {
            for e in entries {
                assert_eq!(e.offered(), inter.check_opentype_feature(e.tag), "{group:?} {}", e.tag);
            }
        }
        let fig: Vec<_> = view.groups.iter().find(|(g, _)| *g == FeatureGroup::FigureStyle).unwrap().1.clone();
        assert!(fig.iter().all(|e| !e.offered()));
        assert!(fig.iter().find(|e| e.tag == tag("lnum")).unwrap().inert());
        let fonts = Fonts::load();
        let text = "1,0";
        let s = shape(&fonts, INTER, "Latn", text, &map, false);
        assert_eq!(s.text, text.as_bytes());
        assert_eq!(s.state("lnum"), FeatureState::UnavailableInert);
        assert_eq!(s.state("tnum"), FeatureState::Applied);
        assert_eq!(FeatureMap::from_features(&s.receipts).unwrap(), map, "unavailable tag not dropped");
    });
}

#[test]
fn mt_31141_enumeration_members_are_exact() {
    let rows: Vec<(&str, &str, Vec<String>)> = StudioFeatureGroup::ALL
        .iter()
        .map(|g| (g.label(), g.meaning(), g.tags().iter().map(|t| t.as_str().to_string()).collect()))
        .collect();
    let v = |tags: &[&str]| tags.iter().map(|t| t.to_string()).collect::<Vec<_>>();
    let ss: Vec<String> = (1..=20).map(|n| format!("ss{n:02}")).collect();
    assert_eq!(
        rows,
        vec![
            ("Ligatures", "Standard, discretionary, and contextual ligatures", v(&["liga", "dlig", "clig", "rlig"])),
            ("Alternates", "Stylistic alternates, contextual alternates, titling alternates, swash, ornaments", v(&["salt", "calt", "titl", "swsh", "ornm"])),
            ("Stylistic sets", "Named/numbered stylistic set toggles", ss),
            ("Figure style", "Lining vs oldstyle numerals", v(&["lnum", "onum"])),
            ("Figure width", "Proportional vs tabular numerals", v(&["pnum", "tnum"])),
            ("Fractions", "Diagonal fractions and stacked fractions", v(&["frac", "afrc"])),
            ("Ordinals", "Ordinal indicators", v(&["ordn"])),
            ("Superscript / subscript", "True OpenType figure/letter positions", v(&["sups", "subs"])),
            ("Numerator / denominator", "Fraction building blocks", v(&["numr", "dnom"])),
            ("Slashed zero", "Disambiguated zero", v(&["zero"])),
            ("Caps forms", "Small caps, all-small-caps, petite caps, capital spacing/forms", v(&["smcp", "c2sc", "pcap", "cpsp", "case"])),
            ("Positional forms", "Initial/medial/final/isolated forms for connected scripts", v(&["init", "medi", "fina", "isol"])),
            ("Kerning / marks", "OpenType kerning, mark positioning", v(&["kern", "mark", "mkmk"])),
        ]
    );
    // Selections decode into the one tag map; non-members refuse; custom tags stay accepted.
    let mut map = FeatureMap::new();
    for g in StudioFeatureGroup::ALL {
        for t in g.tags() {
            g.select(&mut map, t, ON).unwrap();
        }
    }
    assert_eq!(map.len(), 4 + 5 + 20 + 2 + 2 + 2 + 1 + 2 + 2 + 1 + 5 + 4 + 3);
    let before = map.clone();
    assert_eq!(StudioFeatureGroup::Ligatures.select(&mut map, tag("hlig"), ON), Err(FeatureError::TagNotInGroup));
    assert_eq!(map, before);
    map.set(tag("xyz1"), ON).unwrap();
    assert_eq!(StudioFeatureGroup::SlashedZero.selection(&map), vec![(tag("zero"), ON)]);
}

#[test]
fn mt_31141_group_selections_substitute_real_glyphs() {
    bounded_case("mt_31141_group_selections_substitute_real_glyphs", || {
        let fonts = Fonts::load();
        // (group, tag, text, expected glyphs): fontTools single/ligature substitution targets.
        let cases: &[(StudioFeatureGroup, &str, &str, &[u32])] = &[
            (StudioFeatureGroup::Ligatures, "dlig", "!?", &[1338]),
            (StudioFeatureGroup::Alternates, "salt", "1", &[1308]),
            (StudioFeatureGroup::StylisticSets, "ss02", "0", &[1307]),
            (StudioFeatureGroup::FigureWidth, "tnum", ",", &[1412]),
            (StudioFeatureGroup::Ordinals, "ordn", "No.", &[1698]),
            (StudioFeatureGroup::SuperscriptSubscript, "sups", "(", &[1518]),
            (StudioFeatureGroup::SuperscriptSubscript, "subs", "(", &[1561]),
            (StudioFeatureGroup::NumeratorDenominator, "numr", "(", &[1607]),
            (StudioFeatureGroup::NumeratorDenominator, "dnom", "(", &[1592]),
            (StudioFeatureGroup::SlashedZero, "zero", "0", &[1307]),
            (StudioFeatureGroup::CapsForms, "case", "(", &[1349]),
        ];
        for (group, t, text, want) in cases {
            let mut map = FeatureMap::new();
            group.select(&mut map, tag(t), ON).unwrap();
            let s = shape(&fonts, INTER, "Latn", text, &map, false);
            assert_eq!(&s.glyphs, want, "{group:?} {t}");
            assert_eq!(s.state(t), FeatureState::Applied);
            assert_eq!(s.text, text.as_bytes());
        }
        // Groups Inter does not declare stay unavailable and stored.
        for (group, t) in [(StudioFeatureGroup::FigureStyle, "onum"), (StudioFeatureGroup::PositionalForms, "init")] {
            let mut map = FeatureMap::new();
            group.select(&mut map, tag(t), ON).unwrap();
            let s = shape(&fonts, INTER, "Latn", "0", &map, false);
            assert_eq!(s.state(t), FeatureState::UnavailableInert);
        }
        // Positional forms apply on the Arabic face, which declares init.
        let mut map = FeatureMap::new();
        StudioFeatureGroup::PositionalForms.select(&mut map, tag("init"), ON).unwrap();
        let s = shape(&fonts, ARABIC, "Arab", "\u{0628}\u{0628}", &map, false);
        assert_eq!(s.state("init"), FeatureState::Applied);
    });
}

#[test]
fn descriptor_names_the_feature_surface() {
    let d: Value = serde_json::from_str(descriptor()).unwrap();
    let f = &d["features"];
    for key in ["storage", "registry", "groups", "availability", "projections", "catalogs", "diagnostics"] {
        assert!(f[key].is_string(), "{key}");
    }
    assert_eq!(f["tests"], json!(["opentype_features", "character_catalogs"]));
}
