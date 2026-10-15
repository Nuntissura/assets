//! Library mapping slice B (lane SMB-LIGHTROOM-CATALOG): read-only XMP packet decode and the
//! library fixture end to end (set nesting, collection and keyword cycles, smart rules incl. a
//! refused rule bomb, synonyms, plain / compressed-looking / foreign / DOCTYPE XMP values).
//! Byte-decoding proof on a synthetic fixture only; Lightroom parity is UNVERIFIED (D5).
use hsk_studio_accord::CancellationToken;
use hsk_studio_interop_lrcat::{
    ColorLabel, Combine, Disposition, Flag, Grant, HierarchyStatus, LibraryCollectionKind,
    LibraryLimits, LibraryProposal, LimitKind, Limits, LrcatError, RuleNode, SliceSource,
    XmpError, XmpLimits, XmpRating, decode_xmp_packet, extract_catalog, extract_library,
};
use sha2::{Digest, Sha256};

const LIBRARY: &[u8] = include_bytes!("fixtures/synthetic-library.lrcat");
const PROVENANCE: &str = include_str!("fixtures/PROVENANCE-library.json");

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
    Grant::main_only(Box::new(SliceSource::new(LIBRARY.to_vec())))
}

fn lib_with(limits: &Limits) -> Result<LibraryProposal, LrcatError> {
    extract_library(&grant(), limits, &LibraryLimits::default(), &CancellationToken::default())
}

fn loss(p: &LibraryProposal, entity: &str, code: &str) -> (u64, Disposition) {
    let l = p.loss(entity, code).unwrap_or_else(|| panic!("loss {entity}/{code}"));
    (l.count, l.disposition)
}

#[test]
fn library_fixture_end_to_end() {
    bounded_case("library_fixture", || {
        let sha: String = Sha256::digest(LIBRARY).iter().map(|b| format!("{b:02x}")).collect();
        assert!(PROVENANCE.contains(&sha), "fixture bytes match PROVENANCE-library.json");
        let p = lib_with(&Limits::default()).unwrap();
        assert_eq!(p.catalog, extract_catalog(&grant(), &Limits::default(), &CancellationToken::default()).unwrap());

        // Collections: kinds, set nesting, cycle, non-set parent, rules decoded / refused.
        let rows: Vec<(i64, LibraryCollectionKind, Option<i64>, u32, HierarchyStatus)> = p
            .collections
            .iter()
            .map(|c| (c.local_id, c.kind.clone(), c.hierarchy.parent, c.hierarchy.depth, c.hierarchy.status))
            .collect();
        use HierarchyStatus::*;
        use LibraryCollectionKind::*;
        assert_eq!(
            rows,
            [
                (1, CollectionSet, None, 0, Root),
                (2, Collection, Some(1), 1, Child),
                (3, SmartCollection, Some(1), 1, Child),
                (4, SmartCollection, None, 0, Root),
                (5, Collection, None, 0, ParentCycle),
                (6, Collection, None, 0, ParentCycle),
                (7, Collection, Some(2), 2, Child),
            ]
        );
        let best = p.collections[2].rules.as_ref().unwrap();
        assert_eq!(best.combine, Some(Combine::All));
        assert!(matches!(&best.children[1], RuleNode::Group(g) if g.combine == Some(Combine::Any) && g.children.len() == 2));
        assert!(p.collections[3].rules.is_none());
        let e = "AgLibraryCollection";
        assert_eq!(loss(&p, e, "parent_cycle"), (2, Disposition::Approximate));
        assert_eq!(loss(&p, e, "parent_not_collection_set"), (1, Disposition::Approximate));
        assert_eq!(loss(&p, e, "smart_rules_absent"), (1, Disposition::Missing));
        let c = "AgLibraryCollectionContent";
        assert_eq!(loss(&p, c, "rule_depth_exceeded"), (1, Disposition::Unsupported));
        assert_eq!(loss(&p, c, "orphan_row"), (1, Disposition::Unsupported));
        assert_eq!(loss(&p, c, "collection_content_module_unmapped"), (1, Disposition::Unsupported));
        let n = p.count(c).unwrap();
        assert_eq!((n.rows_seen, n.mapped, n.lost), (4, 1, 3));

        // Keywords: chain, cycle, dangling parent, synonyms (orphan synonym refused).
        type KeywordRow<'a> = (i64, Option<i64>, u32, HierarchyStatus, Vec<&'a str>);
        let kw: Vec<KeywordRow> = p
            .keywords
            .iter()
            .map(|k| {
                let syn = k.synonyms.iter().map(String::as_str).collect();
                (k.local_id, k.hierarchy.parent, k.hierarchy.depth, k.hierarchy.status, syn)
            })
            .collect();
        assert_eq!(
            kw,
            [
                (1, None, 0, Root, vec![]),
                (2, Some(1), 1, Child, vec![]),
                (3, Some(2), 2, Child, vec!["Italia", "Italie"]),
                (4, Some(3), 3, Child, vec!["Roma"]),
                (5, None, 0, ParentCycle, vec![]),
                (6, None, 0, ParentCycle, vec![]),
                (7, None, 0, ParentUnresolved, vec![]),
            ]
        );
        let s = p.count("AgLibraryKeywordSynonym").unwrap();
        assert_eq!((s.rows_seen, s.mapped, s.lost), (4, 3, 1));

        // Photos: flags, labels (custom text kept + loss), XMP per image.
        let flags: Vec<Flag> = p.photos.iter().map(|s| s.flag).collect();
        assert_eq!(flags[1], Flag::Pick);
        assert_eq!(flags[2], Flag::Reject);
        assert_eq!(p.photos[4].label, Some(ColorLabel::Custom("Approved".to_owned())));
        assert_eq!(loss(&p, "Adobe_images", "color_label_custom_text"), (1, Disposition::Approximate));
        let x1 = p.photos[0].xmp.as_ref().unwrap();
        assert_eq!((x1.rating, x1.label.as_deref()), (Some(XmpRating::Stars(3)), Some("Red")));
        assert_eq!(x1.keywords, ["Rome", "Tom & Jerry"]);
        assert_eq!(x1.hierarchical_keywords, [vec!["Places", "Europe", "Italy", "Rome"]]);
        assert_eq!(p.photos[1].xmp.as_ref().unwrap().rating, Some(XmpRating::Stars(5)));
        assert!(p.photos[2..].iter().all(|s| s.xmp.is_none()));
        let m = "Adobe_AdditionalMetadata";
        assert_eq!(loss(&p, m, "xmp_rating_differs_from_catalog"), (1, Disposition::Approximate));
        // A zlib-looking value is now inflated through the shared DEFLATE; this one is not a valid stream.
        assert_eq!(loss(&p, m, "xmp_inflate_failed"), (1, Disposition::Unsupported));
        assert_eq!(loss(&p, m, "xmp_not_packet"), (1, Disposition::Unsupported));
        assert_eq!(loss(&p, m, "xmp_doctype_refused"), (1, Disposition::Unsupported));
        assert_eq!(loss(&p, m, "orphan_row"), (1, Disposition::Unsupported));
        let x = p.count(m).unwrap();
        assert_eq!((x.rows_seen, x.mapped, x.lost), (6, 2, 4));
        assert!(p.resolves.contains(&"xmp_blob_not_inflated"));

        // Aggregate: XMP, rules and synonyms are charged against the extraction-wide budget.
        let fits = |budget: u64| {
            let l = Limits { max_retained_text_bytes: budget, ..Limits::default() };
            extract_catalog(&grant(), &l, &CancellationToken::default()).is_ok()
        };
        let (mut lo, mut hi) = (0u64, 16 * 1024 * 1024);
        assert!(fits(hi) && !fits(lo));
        while hi - lo > 1 {
            let mid = lo + (hi - lo) / 2;
            if fits(mid) { hi = mid } else { lo = mid }
        }
        let tight = Limits { max_retained_text_bytes: hi, ..Limits::default() };
        assert_eq!(lib_with(&tight).unwrap_err(), LrcatError::Limit(LimitKind::RetainedText));
        assert_eq!(lib_with(&Limits::default()).unwrap(), p, "deterministic");
    });
}

fn packet(description: &str) -> String {
    format!(
        "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">{description}</rdf:RDF></x:xmpmeta>"
    )
}

#[test]
fn xmp_fields_namespace_and_adversarial_xml() {
    bounded_case("xmp_adversarial", || {
        let d = |t: &str| decode_xmp_packet(t.as_bytes(), &XmpLimits::default());
        // Namespace URI decides, not the prefix: `foo` bound to the XMP URI is xmp:Rating.
        let f = d(&packet(r#"<rdf:Description xmlns:foo="http://ns.adobe.com/xap/1.0/" foo:Rating="4"/>"#)).unwrap();
        assert_eq!(f.rating, Some(XmpRating::Stars(4)));
        // Prefix decoy: `xmp` bound to a foreign URI is not XMP; wrong case is a different name.
        let f = d(&packet(r#"<rdf:Description xmlns:xmp="http://example.com/fake/" xmp:Label="Fake" xmp:Rating="5"/>"#)).unwrap();
        assert_eq!((f.rating, f.label), (None, None));
        let f = d(&packet(r#"<rdf:Description xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmp:rating="2"><XMP:Rating>3</XMP:Rating></rdf:Description>"#)).unwrap();
        assert_eq!(f.rating, None);
        // Default namespace on the element itself, single quotes, whitespace around `=`.
        let f = d(&packet(r#"<Description xmlns="http://www.w3.org/1999/02/22-rdf-syntax-ns#" xmlns:xmp='http://ns.adobe.com/xap/1.0/' xmp:Rating = '-1' />"#)).unwrap();
        assert_eq!(f.rating, Some(XmpRating::Rejected));
        // Comment decoy, CDATA, entity and character references, repeated property.
        let f = d(&packet(concat!(
            r#"<rdf:Description xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmlns:dc="http://purl.org/dc/elements/1.1/" xmp:Rating="2">"#,
            r#"<!-- <xmp:Rating>5</xmp:Rating> <xmp:Label>Decoy</xmp:Label> -->"#,
            r#"<xmp:Rating>4</xmp:Rating>"#,
            r#"<xmp:Label>Blue &amp; Green</xmp:Label>"#,
            r#"<dc:subject><rdf:Bag><rdf:li><![CDATA[a<b & c]]></rdf:li><rdf:li>Tom &amp; Jerry &#x263A; &#65;</rdf:li></rdf:Bag></dc:subject>"#,
            r#"</rdf:Description>"#,
        )))
        .unwrap();
        assert_eq!(f.rating, Some(XmpRating::Stars(2)), "first value kept");
        assert_eq!(f.label.as_deref(), Some("Blue & Green"));
        assert_eq!(f.keywords, ["a<b & c", "Tom & Jerry \u{263A} A"]);
        assert_eq!(f.notes, [("xmp_property_repeated", 1)]);
        // A namespace declared on an inner element is out of scope after it closes.
        let f = d(&packet(concat!(
            r#"<rdf:Description><x:a xmlns:xmp="http://ns.adobe.com/xap/1.0/"/>"#,
            r#"<xmp:Rating>3</xmp:Rating></rdf:Description>"#,
        )))
        .unwrap();
        assert_eq!(f.rating, None);
        for (value, note) in [("2.5", "xmp_rating_out_of_range"), ("abc", "xmp_rating_not_number"), ("9", "xmp_rating_out_of_range")] {
            let t = packet(&format!(r#"<rdf:Description xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmp:Rating="{value}"/>"#));
            assert_eq!(d(&t).unwrap().notes, [(note, 1)]);
        }
        let code = |t: &str| d(t).unwrap_err().code();
        assert_eq!(code(&packet("<rdf:Description>&nbsp;</rdf:Description>")), "xmp_unknown_entity");
        assert_eq!(code(&packet("<rdf:Description>&#0;</rdf:Description>")), "xmp_bad_reference");
        assert_eq!(code("<a><b></a></b>"), "xmp_mismatched_end");
        assert_eq!(code("<a><b>"), "xmp_unclosed_element");
        assert_eq!(code("<a x=1/>"), "xmp_unquoted_attribute");
        assert_eq!(code("<a x=\"<\"/>"), "xmp_lt_in_attribute");
        assert_eq!(code("<!-- open"), "xmp_unterminated_comment");
    });
}

#[test]
fn xmp_bombs_refused_before_allocation() {
    bounded_case("xmp_bombs", || {
        let limits = XmpLimits::default();
        let e = |t: &[u8]| decode_xmp_packet(t, &limits).unwrap_err();
        assert_eq!(e("<a>".repeat(100_000).as_bytes()), XmpError::DepthExceeded);
        assert_eq!(e(format!("<r>{}</r>", "<a/>".repeat(100_000)).as_bytes()), XmpError::NodesExceeded);
        let attrs: String = (0..70_000).map(|i| format!(" a{i}=\"\"")).collect();
        assert_eq!(e(format!("<r{attrs}/>").as_bytes()), XmpError::NodesExceeded);
        let bag = |items: &str| {
            packet(&format!(
                r#"<rdf:Description xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:subject><rdf:Bag>{items}</rdf:Bag></dc:subject></rdf:Description>"#
            ))
        };
        assert_eq!(e(bag(&"<rdf:li>k</rdf:li>".repeat(5_000)).as_bytes()), XmpError::KeywordsExceeded);
        assert_eq!(e(bag(&format!("<rdf:li>{}</rdf:li>", "k".repeat(70_000))).as_bytes()), XmpError::TextTooLong);
        assert_eq!(e(bag(&format!("<rdf:li><![CDATA[{}]]></rdf:li>", "k".repeat(70_000))).as_bytes()), XmpError::TextTooLong);
        assert_eq!(e(format!("<a b=\"{}\"/>", "&amp;".repeat(70_000)).as_bytes()), XmpError::TextTooLong);
        assert_eq!(e(b"<!DOCTYPE a [<!ENTITY x \"xxxxxxxx\">]><a>&x;&x;</a>"), XmpError::DoctypeRefused);
        assert_eq!(e(b"<a>\xff</a>"), XmpError::NotUtf8);
        let tiny = XmpLimits { max_input_bytes: 16, ..limits };
        assert_eq!(decode_xmp_packet(&[b' '; 17], &tiny).unwrap_err(), XmpError::InputTooLarge);
        for bad in [
            XmpLimits { max_input_bytes: u32::MAX, ..limits },
            XmpLimits { max_depth: u32::MAX, ..limits },
            XmpLimits { max_nodes: u32::MAX, ..limits },
            XmpLimits { max_keywords: u32::MAX, ..limits },
            XmpLimits { max_text_bytes: u32::MAX, ..limits },
            XmpLimits { max_depth: 0, ..limits },
        ] {
            assert_eq!(decode_xmp_packet(b"<a/>", &bad).unwrap_err(), XmpError::Configuration);
        }
        let bad_lib = LibraryLimits { xmp: XmpLimits { max_nodes: 0, ..limits }, ..LibraryLimits::default() };
        assert_eq!(
            extract_library(&grant(), &Limits::default(), &bad_lib, &CancellationToken::default()).unwrap_err(),
            LrcatError::Limit(LimitKind::Configuration)
        );
    });
}
