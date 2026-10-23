//! Library import mapping (lane SMB-LIGHTROOM-CATALOG): typed flags/labels/collection kinds,
//! validated hierarchies, smart-rule trees, hostile rule text and aggregate retention.
//! Byte-decoding proof on synthetic fixtures only; Lightroom parity is UNVERIFIED (D5).
use hsk_studio_accord::CancellationToken;
use hsk_studio_interop_lrcat::{
    ColorLabel, Combine, Disposition, Flag, Grant, HierarchyStatus, KnownField, KnownOp,
    LibraryCollectionKind, LibraryLimits, Limits, LimitKind, LrcatError, RuleError, RuleLimits,
    RuleNode, RuleScalar, SliceSource, extract_catalog, extract_library, parse_smart_rules,
    resolve_hierarchy,
};

const MAIN: &[u8] = include_bytes!("fixtures/synthetic-main.lrcat");

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

fn grant() -> Grant {
    Grant::main_only(Box::new(SliceSource::new(MAIN.to_vec())))
}

#[test]
fn library_mapping_keeps_catalog_and_types_state() {
    bounded_case("library_mapping", || {
        let cancel = CancellationToken::default();
        let lib = extract_library(&grant(), &Limits::default(), &LibraryLimits::default(), &cancel).unwrap();
        let catalog = extract_catalog(&grant(), &Limits::default(), &cancel).unwrap();
        // The fixed extraction is untouched by the library layer.
        assert_eq!(lib.catalog, catalog);
        assert!(lib.catalog.reconciles());

        assert_eq!(lib.photos.len(), catalog.photos.len());
        let (mut picks, mut rejects, mut red) = (0, 0, 0);
        for (state, p) in lib.photos.iter().zip(&catalog.photos) {
            assert_eq!(state.local_id, p.local_id);
            assert_eq!(state.rating.map(i64::from), p.rating);
            match (state.flag, p.pick) {
                (Flag::Pick, Some(1)) => picks += 1,
                (Flag::Reject, Some(-1)) => rejects += 1,
                (Flag::Unflagged, Some(0) | None) => {}
                other => panic!("flag mismatch {other:?}"),
            }
            match (&state.label, p.color_label.as_deref()) {
                (Some(ColorLabel::Red), Some("Red")) => red += 1,
                (None, None | Some("")) => {}
                other => panic!("label mismatch {other:?}"),
            }
        }
        assert!(picks > 0 && rejects > 0 && red > 0, "fixture exercises every flag and a label");
        assert!(lib.loss("Adobe_images", "color_label_custom_text").is_none());

        let kinds: Vec<(i64, LibraryCollectionKind)> =
            lib.collections.iter().map(|c| (c.local_id, c.kind.clone())).collect();
        assert_eq!(
            kinds,
            [
                (1, LibraryCollectionKind::Collection),
                (2, LibraryCollectionKind::SmartCollection),
                (3, LibraryCollectionKind::CollectionSet)
            ]
        );
        // Fixture rule text `s = { rule = 1 }`: decoded, nothing typed, the unknown field is a loss.
        let smart = &lib.collections[1];
        let rules = smart.rules.as_ref().expect("smart collection carries its decoded rules");
        assert!(rules.children.is_empty() && rules.combine.is_none());
        let unmapped = lib.loss("AgLibraryCollectionContent", "smart_rule_field_unmapped").unwrap();
        assert_eq!((unmapped.count, unmapped.disposition), (1, Disposition::Approximate));
        assert!(lib.loss("AgLibraryCollectionContent", "smart_rule_combine_absent").is_some());
        let content = lib.count("AgLibraryCollectionContent").unwrap();
        assert_eq!((content.rows_seen, content.mapped, content.lost), (1, 1, 0));
        let synonyms = lib.loss("AgLibraryKeywordSynonym", "table_absent").unwrap();
        assert_eq!(synonyms.disposition, Disposition::Missing);
        for code in ["smart_collection_rules_not_decoded", "collection_kind_unmapped", "collection_content_not_decoded"] {
            assert!(lib.resolves.contains(&code), "resolves {code}");
            assert!(catalog.losses.iter().any(|l| l.code == code), "catalog keeps {code}");
        }

        let tree: Vec<(i64, Option<i64>, u32, HierarchyStatus)> = lib
            .keywords
            .iter()
            .map(|k| (k.local_id, k.hierarchy.parent, k.hierarchy.depth, k.hierarchy.status))
            .collect();
        assert_eq!(tree, [(1, None, 0, HierarchyStatus::Root), (2, Some(1), 1, HierarchyStatus::Child)]);
        // Deterministic: a second pass over the same bytes is identical.
        let again = extract_library(&grant(), &Limits::default(), &LibraryLimits::default(), &cancel).unwrap();
        assert_eq!(again, lib);
    });
}

#[test]
fn smart_rules_typed_tree_and_adversarial_text() {
    bounded_case("smart_rules_typed", || {
        let text = concat!(
            "s = {\r\n",
            "\t{ criteria = \"rating\", operation = \">=\", value = 4, value2 = 0, },\r\n",
            "\t{\n",
            "\t\t{ criteria = \"keywords\", operation = \"any\", value = \"beach \\\"sunset\\\" \\65\\066\", value2 = \"\", },\n",
            // decoy: rule syntax inside a string is data, not a criterion
            "\t\t{ criteria = \"labelColor\", operation = \"==\", value = 'criteria = \"filename\", }', },\n",
            "\t\tcombine = \"union\",\n",
            "\t},\n",
            // decoys: a closing brace and a criterion inside comments
            "\t-- } criteria = \"filename\"\n",
            "\t--[==[ { criteria = \"x\" } ]] still comment ]==]\n",
            // case decoy: `Criteria` is not `criteria`, so this is a group with unmapped fields
            "\t{ Criteria = \"fileFormat\", operation = \"==\", value = [[RAW]] },\n",
            "\t{ criteria = \"someFutureField\", operation = \"near\", value = -2.5e1, value2 = true },\n",
            "\tcombine = \"intersect\",\n",
            "}\n",
        );
        let rules = parse_smart_rules(text.as_bytes(), &RuleLimits::default()).unwrap();
        let root = &rules.root;
        assert_eq!(root.combine, Some(Combine::All));
        assert_eq!(root.children.len(), 4);
        let RuleNode::Criterion(rating) = &root.children[0] else { panic!("criterion") };
        assert_eq!((rating.field.known, rating.operation.known), (Some(KnownField::Rating), Some(KnownOp::Ge)));
        assert_eq!((&rating.value, &rating.value2), (&RuleScalar::Int(4), &Some(RuleScalar::Int(0))));
        let RuleNode::Group(any) = &root.children[1] else { panic!("nested group") };
        assert_eq!(any.combine, Some(Combine::Any));
        let RuleNode::Criterion(kw) = &any.children[0] else { panic!("criterion") };
        assert_eq!(kw.value, RuleScalar::Str("beach \"sunset\" AB".to_owned()), "escapes decoded first");
        let RuleNode::Criterion(label) = &any.children[1] else { panic!("criterion") };
        assert_eq!(label.field.known, Some(KnownField::LabelColor));
        assert_eq!(label.value, RuleScalar::Str("criteria = \"filename\", }".to_owned()));
        let RuleNode::Group(decoy) = &root.children[2] else { panic!("case decoy is a group") };
        assert!(decoy.children.is_empty() && decoy.combine.is_none());
        let RuleNode::Criterion(future) = &root.children[3] else { panic!("criterion") };
        assert_eq!((future.field.known, future.field.raw.as_str()), (None, "someFutureField"));
        assert_eq!((future.operation.known, future.value.clone()), (None, RuleScalar::Num(-25.0)));
        assert_eq!(
            rules.notes,
            [
                ("smart_rule_combine_absent", 1),
                ("smart_rule_criteria_unrecognised", 1),
                ("smart_rule_field_unmapped", 3),
                ("smart_rule_operation_unrecognised", 1),
            ]
        );
        let syntax = |t: &str| parse_smart_rules(t.as_bytes(), &RuleLimits::default()).map(|_| ()).unwrap_err().code();
        assert_eq!(syntax("{ \"\\q\" }"), "rule_bad_escape");
        assert_eq!(syntax("{ \"abc"), "rule_unterminated_string");
        assert_eq!(syntax("{ \"\\300\" }"), "rule_bad_escape");
        assert_eq!(syntax("{ x }"), "rule_unexpected_token");
        assert_eq!(syntax("{ } os.exit()"), "rule_trailing_text");
        assert_eq!(syntax("\"just a string\""), "rule_not_a_table");
        assert!(parse_smart_rules(b"{ -- }\n }", &RuleLimits::default()).unwrap().root.children.is_empty());
    });
}

#[test]
fn smart_rule_bombs_refused_before_allocation() {
    bounded_case("rule_bombs", || {
        let limits = RuleLimits::default();
        let err = |t: &[u8]| parse_smart_rules(t, &limits).map(|_| ()).unwrap_err();
        let deep = [vec![b'{'; 100_000], vec![b'}'; 100_000]].concat();
        assert_eq!(err(&deep), RuleError::DepthExceeded);
        let wide = format!("{{{}}}", "1,".repeat(10_000));
        assert_eq!(err(wide.as_bytes()), RuleError::NodesExceeded);
        let long = format!("{{ \"{}\" }}", "a".repeat(5_000));
        assert_eq!(err(long.as_bytes()), RuleError::StringTooLong);
        let bracket = format!("{{ [[{}]] }}", "a".repeat(5_000));
        assert_eq!(err(bracket.as_bytes()), RuleError::StringTooLong);
        assert_eq!(err(format!("{{ {} }}", "9".repeat(100)).as_bytes()).code(), "rule_number_too_long");
        assert_eq!(err(b"{ 0xFFFFFFFFFFFFFFFFFF }").code(), "rule_bad_number");
        let tiny = RuleLimits { max_input_bytes: 16, ..limits };
        assert_eq!(parse_smart_rules(&[b' '; 17], &tiny).unwrap_err(), RuleError::InputTooLarge);
        // Limits at the numeric maximum and zero are refused as configuration, before any read.
        for bad in [
            RuleLimits { max_input_bytes: u32::MAX, ..limits },
            RuleLimits { max_depth: u32::MAX, ..limits },
            RuleLimits { max_nodes: u32::MAX, ..limits },
            RuleLimits { max_string_bytes: u32::MAX, ..limits },
            RuleLimits { max_nodes: 0, ..limits },
        ] {
            assert_eq!(parse_smart_rules(b"{}", &bad).unwrap_err(), RuleError::Configuration);
        }
        let deep_cap = LibraryLimits { max_hierarchy_depth: u32::MAX, ..LibraryLimits::default() };
        let cancel = CancellationToken::default();
        assert_eq!(
            extract_library(&grant(), &Limits::default(), &deep_cap, &cancel).unwrap_err(),
            LrcatError::Limit(LimitKind::Configuration)
        );
        // Aggregate: the smallest retained budget the fixed extraction fits in is too small once
        // the library layer's retained rules, states and hierarchy entries are charged too.
        let fits = |budget: u64| {
            let l = Limits { max_retained_text_bytes: budget, ..Limits::default() };
            extract_catalog(&grant(), &l, &cancel).is_ok()
        };
        let (mut lo, mut hi) = (0u64, 64 * 1024 * 1024);
        assert!(fits(hi) && !fits(lo));
        while hi - lo > 1 {
            let mid = lo + (hi - lo) / 2;
            if fits(mid) { hi = mid } else { lo = mid }
        }
        let tight = Limits { max_retained_text_bytes: hi, ..Limits::default() };
        assert_eq!(
            extract_library(&grant(), &tight, &LibraryLimits::default(), &cancel).unwrap_err(),
            LrcatError::Limit(LimitKind::RetainedText)
        );
        let roomy = Limits { max_retained_text_bytes: hi * 4, ..Limits::default() };
        assert!(extract_library(&grant(), &roomy, &LibraryLimits::default(), &cancel).is_ok());
    });
}

#[test]
fn hierarchy_cycles_unresolved_and_depth_cap() {
    bounded_case("hierarchy", || {
        let items = [
            (1, None),
            (2, Some(1)),
            (3, Some(4)),
            (4, Some(3)),
            (5, Some(5)),
            (6, Some(99)),
            (7, Some(3)),
            (2, Some(7)),
        ];
        let got: Vec<(i64, Option<i64>, u32, HierarchyStatus)> = resolve_hierarchy(&items, 8)
            .iter()
            .map(|h| (h.local_id, h.parent, h.depth, h.status))
            .collect();
        use HierarchyStatus::*;
        assert_eq!(
            got,
            [
                (1, None, 0, Root),
                (2, Some(1), 1, Child),
                (3, None, 0, ParentCycle),
                (4, None, 0, ParentCycle),
                (5, None, 0, ParentCycle),
                (6, None, 0, ParentUnresolved),
                (7, Some(3), 1, Child),
                (2, None, 0, DuplicateId),
            ]
        );
        // A 100k-deep keyword chain resolves in linear time; nodes past the cap are flagged.
        let n: i64 = 100_000;
        let chain: Vec<(i64, Option<i64>)> = (0..n).map(|i| (i, (i > 0).then(|| i - 1))).collect();
        let forward = resolve_hierarchy(&chain, 256);
        let exceeded = forward.iter().filter(|h| h.status == DepthExceeded).count();
        assert_eq!(exceeded as i64, n - 257);
        assert_eq!(forward[99_999].depth, 99_999);
        // Input order does not change any node's result.
        let reversed: Vec<(i64, Option<i64>)> = chain.iter().rev().copied().collect();
        let mut backward = resolve_hierarchy(&reversed, 256);
        backward.reverse();
        assert_eq!(backward, forward);
        // A long cycle is detected once and detached as a whole.
        let ring: Vec<(i64, Option<i64>)> = (0..n).map(|i| (i, Some((i + 1) % n))).collect();
        assert!(resolve_hierarchy(&ring, 256).iter().all(|h| h.status == ParentCycle && h.parent.is_none()));
    });
}
