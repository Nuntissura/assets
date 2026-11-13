//! Lane SMB-TYPE-CHARACTER-ATTRIBUTES-1, review FIX_FIRST of 83cb89b8, Folio half: binary features are
//! booleans (STU-TYP-170, F13 c), language names are range keys (F7), and the one resolver (F3 + Q2, F8):
//! a style's legacy size, leading and letter spacing are folded into its own record level, so STU-TYP-201
//! order holds across both forms; resolution is bounded in based_on depth and in work, typed past the bound.
use hsk_studio_folio::*;
use serde_json::json;

fn identity(prefix: &str, n: u32) -> String {
    format!("{prefix}-019abcde-0000-7000-8000-{n:012x}")
}
fn style(n: u32, scope: &str, based_on: Option<u32>, extra: serde_json::Value) -> StudioTypeStyle {
    let mut v = json!({"schema_id": "hsk.studio.type_style@1", "style_id": identity("STYS", n), "scope": scope, "name": format!("s{n}")});
    if let Some(b) = based_on {
        v["based_on"] = json!(identity("STYS", b));
    }
    for (k, x) in extra.as_object().cloned().unwrap_or_default() {
        v[k] = x;
    }
    serde_json::from_value(v).expect("style shape")
}
fn pt(v: f64) -> serde_json::Value {
    json!({"value": v, "unit": "pt"})
}

#[test]
fn binary_features_are_booleans_and_alternates_integers() {
    let a: Result<StudioCharacterAttributes, _> = serde_json::from_value(json!({"opentype_features": {"liga": true, "kern": false, "salt": 3}}));
    let a = a.expect("a binary feature is a boolean (STU-TYP-170)");
    assert_eq!(serde_json::to_value(&a).unwrap(), json!({"opentype_features": {"kern": false, "liga": true, "salt": 3}}));
}

#[test]
fn a_language_name_is_a_range_key() {
    let lang = |name: &str| StudioLanguage {
        name: name.into(),
        primary_language_name: "English".into(),
        sublanguage_name: None,
        untranslated_name: None,
        icu_locale_name: "en_US".into(),
        hyphenation_vendor: None,
        spelling_vendor: None,
        single_quotes: None,
        double_quotes: None,
    };
    assert!(lang("en_US").validate().is_ok());
    for bad in ["English: USA", "en US", "", "x".repeat(129).as_str()] {
        assert_eq!(lang(bad).validate().map_err(|d| d.code), Err(Code::InvalidField), "{bad:?}");
    }
}

/// STU-TYP-201: the character style's legacy size beats the paragraph style's record size; a style's
/// record beats its own legacy field; legacy line height and letter spacing fold as leading and tracking.
#[test]
fn legacy_fields_fold_into_their_own_level() {
    let mut r = StudioTypeStyleRecords::new(identity("SDOC", 1));
    r.styles.push(style(1, "paragraph", None, json!({"font_size": pt(10.0), "line_height": {"line_height_kind": "length", "length": pt(14.0)}})));
    r.styles.push(style(2, "character", None, json!({"font_size": pt(18.0), "letter_spacing": {"spacing_kind": "percent", "value": 5.0}})));
    r.styles.push(style(3, "character", None, json!({"font_size": pt(9.0), "letter_spacing": {"spacing_kind": "length", "length": pt(0.9)}})));
    r.style_character.push(StudioStyleCharacter { style_id: identity("STYS", 1), attributes: StudioCharacterAttributes { size: Some(11.0), ..Default::default() } });
    let mut res = CharacterResolver::new(&r, 1 << 20);
    let story = identity("STXT", 1);
    let p = res.at(&story, 0, Some(&identity("STYS", 1)), None).unwrap();
    assert_eq!((p.size, p.leading), (Some(11.0), Some(14.0)), "the record beats its own style's legacy size");
    let c = res.at(&story, 0, Some(&identity("STYS", 1)), Some(&identity("STYS", 2))).unwrap();
    assert_eq!((c.size, c.leading, c.tracking), (Some(18.0), Some(14.0), Some(50.0)), "5 % letter spacing is 50 em/1000");
    let d = res.at(&story, 0, None, Some(&identity("STYS", 3))).unwrap();
    assert_eq!(d.tracking.map(|t| (t * 1000.0).round() / 1000.0), Some(100.0), "0.9 pt at 9 pt is 100 em/1000");
}

/// F8: a based_on chain past MAX_STYLE_CHAIN is refused typed, without walking the whole chain.
#[test]
fn a_deep_based_on_chain_is_refused_typed() {
    let mut r = StudioTypeStyleRecords::new(identity("SDOC", 1));
    let depth = 4096u32;
    for n in 1..=depth {
        r.styles.push(style(n, "character", (n > 1).then(|| n - 1), json!({})));
    }
    let mut res = CharacterResolver::new(&r, 1 << 20);
    let e = res.at(&identity("STXT", 1), 0, None, Some(&identity("STYS", depth))).unwrap_err();
    assert_eq!(e.code, Code::BudgetExceeded);
    assert!(res.steps() <= (MAX_STYLE_CHAIN as u64) * 2, "bounded walk: {}", res.steps());
    let ok = res.at(&identity("STXT", 1), 0, None, Some(&identity("STYS", MAX_STYLE_CHAIN as u32))).map(|a| a.is_empty());
    assert_eq!(ok, Ok(true), "a chain at the bound resolves");
}

/// F8: override lookups are indexed (logarithmic in the overrides of the story), counted, and refused typed
/// past the caller's work budget.
#[test]
fn override_lookups_are_indexed_and_budgeted() {
    let mut r = StudioTypeStyleRecords::new(identity("SDOC", 1));
    let n = 65_536u64;
    let story = identity("STXT", 1);
    r.character_overrides = (0..n).map(|i| StudioCharacterOverride { text_id: story.clone(), start: i, end: i + 1, attributes: StudioCharacterAttributes { tracking: Some((i % 7) as f64), ..Default::default() } }).collect();
    let mut res = CharacterResolver::new(&r, 1 << 24);
    for i in (0..n).step_by(7) {
        assert_eq!(res.at(&story, i, None, None).unwrap().tracking, Some(0.0));
    }
    let lookups = n.div_ceil(7);
    assert!(res.steps() > 0 && res.steps() <= lookups * 40, "about log2(65536) steps per lookup: {}", res.steps());
    let mut tight = CharacterResolver::new(&r, 100);
    let refused = (0..n).map(|i| tight.at(&story, i, None, None)).find_map(Result::err);
    assert_eq!(refused.map(|d| d.code), Some(Code::BudgetExceeded));
}
