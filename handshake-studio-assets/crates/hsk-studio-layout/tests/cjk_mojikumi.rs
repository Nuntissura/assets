//! Lane SMB-ID-CJK-KINSOKU-1, slice 3: mojikumi class-pair aki as glue over the seventeen base
//! sets (STU-TYP-138), aki overrides and determinism (STU-TYP-140). Synthetic 10 pt em boxes, so
//! widths and gaps are closed-form. The aki values are declared readings of W3C JLREQ / CLREQ; the
//! values of InDesign's shipped sets are NOT_PROVEN (no InDesign CJK oracle case, D5).
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{StudioAkiOverride, StudioCompositionScope, StudioMojikumiBaseSet, StudioMojikumiClass as K, StudioMojikumiTable};
use hsk_studio_layout::*;

const EM: f64 = 10.0;

fn para(text: &str) -> ShapedParagraph {
    ShapedParagraph::from_fixed_advance(text, SourceSpan { start: 0, end: text.len() as u64 }, EM, 12.0, 10.0).unwrap()
}
fn lines(text: &str, c: &CjkParagraph, measure: f64, justified: bool) -> Vec<CjkComposedLine> {
    compose_cjk_lines(&para(text), text, c, EM, measure, justified, &CancellationToken::default()).unwrap()
}
fn plain(set: StudioMojikumiBaseSet) -> CjkParagraph {
    CjkParagraph::new(StudioCompositionScope::SingleLine, None, MojikumiRules::base(set))
}

/// All seventeen base sets are shipped and no two compose alike: their rules differ in indent,
/// punctuation widths, mid-line pairs or line-end settings, and each composes a sample paragraph.
#[test]
fn all_seventeen_base_sets_are_shipped_and_distinct() {
    let classes = [K::OpeningBrackets, K::ClosingBrackets, K::Commas, K::FullStops, K::MiddleDots, K::Ideographic, K::Hiragana, K::WesternCharacters];
    let mut signatures = std::collections::BTreeSet::new();
    for set in StudioMojikumiBaseSet::ALL {
        let r = MojikumiRules::base(set);
        assert_eq!(r.base, set);
        let mut sig = format!("{} {} ", r.indent_em, r.opening_start_extra_em);
        for a in classes {
            sig += &format!("{:?}{:?}{:?}|", r.line_end(a), r.line_start(a), r.embedded(a));
            for b in classes {
                sig += &format!("{:?}", r.pair(a, b));
            }
        }
        assert!(signatures.insert(sig), "{set:?} duplicates another set");
        let ls = lines("「漢字」、漢字。漢・字", &plain(set), 45.0, true);
        assert_eq!(ls.last().unwrap().glyph_range.end, 11, "{set:?}");
    }
    assert_eq!(signatures.len(), 17);
    // `nothing` sets every glyph on its shaped advance: no aki, no gaps.
    let ls = lines("「漢字」、漢字。", &plain(StudioMojikumiBaseSet::Nothing), 200.0, false);
    assert!(ls[0].gaps.is_empty() && ls[0].start_offset_pt == 0.0 && ls[0].width_pt == 80.0);
    // Indent sets indent the first line by one em (plus 1/2 em before an opening bracket in the
    // one-or-one-and-a-half sets); the Chinese defaults indent two em.
    let first = |set, text: &str| lines(text, &plain(set), 200.0, false)[0].start_offset_pt;
    assert_eq!(first(StudioMojikumiBaseSet::OneEmIndentLineEndAllOneEm, "漢字"), 10.0);
    assert_eq!(first(StudioMojikumiBaseSet::OneEmIndentLineEndAllOneEm, "「漢」"), 5.0);
    assert_eq!(first(StudioMojikumiBaseSet::OneOrOneHalfEmIndentLineEndAllOneEm, "「漢」"), 10.0);
    assert_eq!(first(StudioMojikumiBaseSet::SimplifiedChineseDefault, "漢字"), 20.0);
    assert_eq!(first(StudioMojikumiBaseSet::LineEndAllOneEm, "「漢」"), -5.0, "half-width opening bracket at the line start");
}

/// `line_end_all_one_half_em` sets a closing bracket that ends a line half-width (no aki after
/// it); `line_end_all_one_em` keeps its 1/2 em. Mid-line both keep the full-width box.
#[test]
fn line_end_half_em_set_compresses_closing_brackets() {
    let half = MojikumiRules::base(StudioMojikumiBaseSet::LineEndAllOneHalfEm);
    let full = MojikumiRules::base(StudioMojikumiBaseSet::LineEndAllOneEm);
    assert_eq!(half.line_end(K::ClosingBrackets), Aki::ZERO);
    assert_eq!(full.line_end(K::ClosingBrackets), Aki { minimum_em: 0.5, desired_em: 0.5, maximum_em: 0.5 });
    let text = "漢漢」漢漢漢";
    let h = lines(text, &plain(StudioMojikumiBaseSet::LineEndAllOneHalfEm), 30.0, false);
    let f = lines(text, &plain(StudioMojikumiBaseSet::LineEndAllOneEm), 30.0, false);
    assert_eq!((h[0].glyph_range.clone(), h[0].width_pt), (0..3, 25.0));
    assert_eq!((f[0].glyph_range.clone(), f[0].width_pt), (0..3, 30.0));
    // Mid-line the bracket occupies its full em in both sets.
    for set in [StudioMojikumiBaseSet::LineEndAllOneHalfEm, StudioMojikumiBaseSet::LineEndAllOneEm] {
        let one = lines(text, &plain(set), 200.0, false);
        assert!(one[0].gaps.is_empty(), "{set:?} {:?}", one[0].gaps);
        assert_eq!(one[0].width_pt, 60.0);
    }
    // Consecutive punctuation shares one aki (JLREQ 3.1.4): "」「" closes up by 1/2 em.
    let ls = lines("漢」「漢", &plain(StudioMojikumiBaseSet::LineEndAllOneHalfEm), 200.0, false);
    assert_eq!(ls[0].gaps, vec![CjkGap { glyph: 1, delta_pt: -5.0 }]);
    // Traditional Chinese keeps every punctuation full-width (no combining).
    let ls = lines("漢」「漢", &plain(StudioMojikumiBaseSet::TraditionalChineseDefault), 200.0, false);
    assert!(ls[0].gaps.is_empty(), "{:?}", ls[0].gaps);
}

/// An `aki_override_list` entry replaces the base set's value for exactly its class pair;
/// `use_cid_mojikumi` is refused (no glyph-CID class mapping).
#[test]
fn aki_override_replaces_the_base_pair() {
    let over = StudioAkiOverride { previous: K::ClosingBrackets, next: K::Ideographic, minimum_em: 0.0, desired_em: 0.25, maximum_em: 0.5 };
    let table = StudioMojikumiTable {
        name: "tight".into(),
        based_on: StudioMojikumiBaseSet::LineEndAllOneHalfEm,
        aki_override_list: vec![over],
        use_cid_mojikumi: false,
    };
    let rules = MojikumiRules::from_table(&table).unwrap();
    let base = MojikumiRules::base(StudioMojikumiBaseSet::LineEndAllOneHalfEm);
    assert_eq!(rules.pair(K::ClosingBrackets, K::Ideographic), Aki { minimum_em: 0.0, desired_em: 0.25, maximum_em: 0.5 });
    assert_eq!(base.pair(K::ClosingBrackets, K::Ideographic), Aki { minimum_em: 0.0, desired_em: 0.5, maximum_em: 0.5 });
    assert_eq!(rules.pair(K::ClosingBrackets, K::Hiragana), base.pair(K::ClosingBrackets, K::Hiragana), "other pairs unchanged");
    let c = CjkParagraph::new(StudioCompositionScope::SingleLine, None, rules);
    assert_eq!(lines("漢」漢", &c, 200.0, false)[0].gaps, vec![CjkGap { glyph: 1, delta_pt: -2.5 }]);
    assert!(lines("漢」漢", &plain(StudioMojikumiBaseSet::LineEndAllOneHalfEm), 200.0, false)[0].gaps.is_empty());
    assert!(lines("漢」ひ", &c, 200.0, false)[0].gaps.is_empty());
    // Line-start override: an opening bracket starting a line keeps 1/2 em before it.
    let start = StudioMojikumiTable {
        aki_override_list: vec![StudioAkiOverride { previous: K::LineStart, next: K::OpeningBrackets, minimum_em: 0.5, desired_em: 0.5, maximum_em: 0.5 }],
        ..table.clone()
    };
    let c = CjkParagraph::new(StudioCompositionScope::SingleLine, None, MojikumiRules::from_table(&start).unwrap());
    let ls = lines("漢漢「漢", &c, 20.0, false);
    assert_eq!(ls[1].glyph_range.start, 2);
    assert_eq!(ls[1].start_offset_pt, 0.0, "full-width bracket box at the line start");
    let cid = StudioMojikumiTable { use_cid_mojikumi: true, ..table };
    assert_eq!(MojikumiRules::from_table(&cid).unwrap_err(), LayoutError::UnsupportedComposition);
}

/// STU-TYP-140: the same paragraph, tables and scope give identical breaks and spacing, also from
/// independently built (equal) settings, in both scopes, justified or not.
#[test]
fn cjk_composition_is_deterministic() {
    let text = "「漢字」、漢字。漢字ー漢字・漢字１２３漢字、漢字漢字。";
    for set in StudioMojikumiBaseSet::ALL {
        for scope in [StudioCompositionScope::SingleLine, StudioCompositionScope::EveryLine] {
            let make = || {
                let mut c = CjkParagraph::new(scope, KinsokuSet::shipped("hard_kinsoku"), MojikumiRules::base(set));
                c.rensuuji = true;
                c.hang = HangMode::Optional;
                c
            };
            for justified in [false, true] {
                let a = lines(text, &make(), 47.0, justified);
                let b = lines(text, &make(), 47.0, justified);
                assert_eq!(format!("{a:?}"), format!("{b:?}"), "{set:?} {scope:?}");
                assert_eq!(a.last().unwrap().glyph_range.end, text.chars().count());
                if justified {
                    // Every line but the last fills the measure unless it is overfull or has no aki.
                    for l in &a[..a.len() - 1] {
                        assert!(l.width_pt <= 47.0 + 1e-9 || l.overfull, "{set:?} {scope:?} {l:?}");
                    }
                }
            }
        }
    }
}
