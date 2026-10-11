//! Foundation contract of the vector interop adapter (MT-35549 case names). Every SVG is a
//! synthetic string; every expected number is computed by hand in the comments.
use hsk_studio_accord::{CancellationToken, DomainId};
use hsk_studio_folio::{Budget, Payload, Unresolved, validate_document};
use hsk_studio_interop_vector::*;
use hsk_studio_nib::{Anchor, PathElement, PathLimits, PathPoint, Point, Winding};
use std::collections::BTreeSet;

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

fn import(svg: &str) -> Result<VectorProposal> {
    import_with(svg, &ImportPolicy::default())
}

fn import_with(svg: &str, policy: &ImportPolicy) -> Result<VectorProposal> {
    import_svg(
        svg.as_bytes(),
        policy,
        &mut SequentialIdMint::new(7),
        &CancellationToken::default(),
    )
}

fn xy(a: &Anchor) -> [(f64, f64); 3] {
    [
        (a.position.x.value(), a.position.y.value()),
        (a.incoming.x.value(), a.incoming.y.value()),
        (a.outgoing.x.value(), a.outgoing.y.value()),
    ]
}

fn assert_anchor(a: &Anchor, expect: [(f64, f64); 3]) {
    for (got, want) in xy(a).into_iter().zip(expect) {
        assert!(
            (got.0 - want.0).abs() < 1e-9 && (got.1 - want.1).abs() < 1e-9,
            "anchor {got:?} != {want:?}"
        );
    }
}

fn assert_same_geometry(a: &[VectorPath], b: &[VectorPath]) {
    assert_eq!(a.len(), b.len());
    for (p, q) in a.iter().zip(b) {
        assert_eq!((p.closed, p.winding), (q.closed, q.winding));
        assert_eq!(p.anchors.len(), q.anchors.len());
        for (x, y) in p.anchors.iter().zip(&q.anchors) {
            assert_anchor(x, xy(y));
        }
    }
}

fn kinds(p: &VectorProposal) -> Vec<(LossKind, &'static str)> {
    p.loss.iter().map(|l| (l.kind, l.code)).collect()
}

/// 1 px = 0.75 pt at 96 ppi. The group translates by (10,20) px, which is baked.
const EDIT_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="200" height="100" viewBox="0 0 200 100">
  <g transform="translate(10,20)" fill="#ff0000" stroke="#0000ff" stroke-width="4">
    <path d="M0 0 C 0 40, 40 40, 40 0 L 80 0 Z" fill-rule="evenodd"/>
    <rect x="100" y="0" width="40" height="20" fill="none"/>
  </g>
</svg>"##;

#[test]
fn svg_edit_transform_style_reopen_twice() {
    bounded_case("svg_edit_transform_style_reopen_twice", || {
        let opts = ExportOptions::default();
        let one = import(EDIT_SVG).unwrap();
        // Group transform is reported once; nothing else is lossy in this file.
        assert_eq!(kinds(&one), vec![(LossKind::TransformBaked, "transform_baked")]);
        assert_eq!(one.paths.len(), 2);
        assert_eq!(one.provenance.px_to_pt, 0.75);

        // Path: px points (10,20) (50,20) (90,20) -> pt (7.5,15) (37.5,15) (67.5,15).
        // First curve controls (10,60) and (50,60) px: relative (0,40) px = (0,30) pt.
        let p = &one.paths[0];
        assert!(p.closed);
        assert_eq!(p.winding, Winding::EvenOdd);
        assert_eq!(p.anchors.len(), 3);
        assert_anchor(&p.anchors[0], [(7.5, 15.0), (0.0, 0.0), (0.0, 30.0)]);
        assert_anchor(&p.anchors[1], [(37.5, 15.0), (0.0, 30.0), (0.0, 0.0)]);
        assert_anchor(&p.anchors[2], [(67.5, 15.0), (0.0, 0.0), (0.0, 0.0)]);
        // Rect, no fill: corners (110,20) (150,20) (150,40) (110,40) px -> pt.
        let r = &one.paths[1];
        assert_eq!((r.closed, r.winding), (true, Winding::None));
        let corners: Vec<_> = r.anchors.iter().map(|a| xy(a)[0]).collect();
        assert_eq!(
            corners,
            vec![(82.5, 15.0), (112.5, 15.0), (112.5, 30.0), (82.5, 30.0)]
        );
        // Style: red fill + blue stroke 4 px = 3 pt, and a stroke-only style for the rect.
        assert_eq!(one.styles.len(), 2);
        let red = one.styles.iter().find(|s| s.fill.is_some()).unwrap();
        assert_eq!(red.fill.unwrap(), Rgba { r: 255, g: 0, b: 0, a: 1.0 });
        let stroke = red.stroke.unwrap();
        assert_eq!((stroke.color.b, stroke.width_pt), (255, 3.0));

        // Export is deterministic; reopen twice keeps geometry, style and winding.
        let text = export_svg(&one.paths, &one.styles, &opts).unwrap();
        assert_eq!(text, export_svg(&one.paths, &one.styles, &opts).unwrap());
        assert!(text.contains("fill-rule=\"evenodd\"") && text.contains("fill=\"none\""));
        let two = import(&text).unwrap();
        let three = import(&export_svg(&two.paths, &two.styles, &opts).unwrap()).unwrap();
        assert_same_geometry(&one.paths, &two.paths);
        assert_same_geometry(&two.paths, &three.paths);
        assert!(!two.has_loss(LossKind::TransformBaked));
        assert_eq!(kinds(&two), kinds(&three));
        let (s2, s3) = (&two.styles, &three.styles);
        assert_eq!(s2.len(), s3.len());
        for (a, b) in s2.iter().zip(s3) {
            assert_eq!((a.fill, a.stroke), (b.fill, b.stroke));
        }

        // An edit (move the middle anchor 6 pt right) survives export and reopen.
        let mut edited = two.paths.clone();
        let a = edited[0].anchors[1];
        edited[0].anchors[1].position = Point::pt(a.position.x.value() + 6.0, a.position.y.value()).unwrap();
        let reopened = import(&export_svg(&edited, &two.styles, &opts).unwrap()).unwrap();
        assert_anchor(&reopened.paths[0].anchors[1], [(43.5, 15.0), (0.0, 30.0), (0.0, 0.0)]);
        assert_anchor(&reopened.paths[0].anchors[0], xy(&two.paths[0].anchors[0]));
        assert_anchor(&reopened.paths[0].anchors[2], xy(&two.paths[0].anchors[2]));
    });
}

#[test]
fn ai_native_editability_or_explicit_blocker() {
    bounded_case("ai_native_editability_or_explicit_blocker", || {
        let mut ai = b"%PDF-1.6\n%\xe2\xe3\xcf\xd3\n1 0 obj\n<</PieceInfo<</Illustrator 4 0 R>>>>\nendobj\n".to_vec();
        ai.extend_from_slice(&[0, 255, 1, 254, b'%', b'%', b'E', b'O', b'F']);
        assert_eq!(
            identify_ai(&ai),
            AiKind::PdfCompatible { has_private_data: true }
        );
        match import_ai(&ai, &ImportPolicy::default(), &CancellationToken::default()) {
            Err(VectorError::NativeEditabilityBlocked { version, reason, preserved }) => {
                assert_eq!(version, None);
                assert!(reason.starts_with("native_ai_decoder_missing"));
                assert_eq!(preserved.kind, "ai_pdf_compatible");
                assert_eq!(preserved.bytes, ai, "original bytes preserved byte-exact");
            }
            other => panic!("expected the explicit blocker, got {other:?}"),
        }
        assert_eq!(
            identify_ai(b"%PDF-1.4\n1 0 obj\n<<>>\nendobj\n"),
            AiKind::PdfCompatible { has_private_data: false }
        );
        assert_eq!(identify_ai(b"%!PS-Adobe-3.0\n%%Creator: x"), AiKind::LegacyPostScript);
        assert_eq!(identify_ai(EDIT_SVG.as_bytes()), AiKind::NotAi);
        assert_eq!(identify_ai(b"%P"), AiKind::Unknown);
        assert_eq!(
            import_ai(EDIT_SVG.as_bytes(), &ImportPolicy::default(), &CancellationToken::default()).unwrap_err(),
            VectorError::Unsupported("not_an_ai_file")
        );
    });
}

#[test]
fn external_entity_link_denial_budget_cancel() {
    bounded_case("external_entity_link_denial_budget_cancel", || {
        // An internal DTD subset with an external entity is refused before any parsing of content.
        let xxe = r#"<?xml version="1.0"?><!DOCTYPE svg [<!ENTITY xxe SYSTEM "file:///nonexistent/secret">]><svg xmlns="http://www.w3.org/2000/svg">&xxe;</svg>"#;
        assert_eq!(import(xxe).unwrap_err(), VectorError::EntityDenied);
        // The classic Illustrator DOCTYPE (external id only) is inert: reported, never fetched.
        let legacy = r#"<?xml version="1.0"?><!DOCTYPE svg PUBLIC "-//W3C//DTD SVG 1.1//EN" "http://example.invalid/svg11.dtd"><svg xmlns="http://www.w3.org/2000/svg"/>"#;
        let p = import(legacy).unwrap();
        assert!(p.has_loss_code("doctype_external_id_ignored"));

        let links = r##"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink">
  <image href="http://example.invalid/a.png" width="10" height="10"/>
  <use xlink:href="file.svg#a"/>
  <image href="data:image/png;base64,AAAA" width="10" height="10"/>
  <use href="#local"/>
</svg>"##;
        let p = import(links).unwrap();
        let denied = p.loss.iter().filter(|l| l.kind == LossKind::ExternalDenied).count();
        assert_eq!(denied, 2, "http image and file use denied; data: and #local are not links");
        assert_eq!(p.loss.iter().filter(|l| l.code == "unsupported_image").count(), 2);
        assert_eq!(p.loss.iter().filter(|l| l.code == "unsupported_use").count(), 2);
        assert!(p.paths.is_empty());
        let url_paint = r#"<svg xmlns="http://www.w3.org/2000/svg"><rect width="5" height="5" fill="url(http://example.invalid/p.svg#x)"/></svg>"#;
        assert!(import(url_paint).unwrap().has_loss(LossKind::ExternalDenied));

        // Budgets are typed failures, never partial proposals.
        let small = |f: fn(&mut Limits)| {
            let mut policy = ImportPolicy::default();
            f(&mut policy.limits);
            policy
        };
        let many: String = (0..50).map(|_| "<path d=\"M0 0 L1 1\"/>").collect();
        let doc = format!("<svg xmlns=\"http://www.w3.org/2000/svg\">{many}</svg>");
        assert_eq!(
            import_with(&doc, &small(|l| l.max_nodes = 10)).unwrap_err(),
            VectorError::BudgetExceeded("nodes")
        );
        let long: String = (0..100).map(|i| format!("L{i} {i} ")).collect();
        let doc = format!("<svg xmlns=\"http://www.w3.org/2000/svg\"><path d=\"M0 0 {long}\"/></svg>");
        assert_eq!(
            import_with(&doc, &small(|l| l.max_segments = 10)).unwrap_err(),
            VectorError::BudgetExceeded("segments")
        );
        let deep = format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\">{}{}</svg>",
            "<g>".repeat(10),
            "</g>".repeat(10)
        );
        assert_eq!(
            import_with(&deep, &small(|l| l.max_depth = 4)).unwrap_err(),
            VectorError::BudgetExceeded("depth")
        );
        assert_eq!(
            import_with(EDIT_SVG, &small(|l| l.max_bytes = 64)).unwrap_err(),
            VectorError::BudgetExceeded("bytes")
        );
        assert_eq!(import("<html/>").unwrap_err(), VectorError::NotSvg);
        assert_eq!(import("<svg xmlns=\"http://www.w3.org/2000/svg\">&nope;</svg>").unwrap_err(), VectorError::MalformedXml);

        // Cancellation is checked at entry and per element.
        let token = CancellationToken::default();
        token.cancel();
        let err = import_svg(EDIT_SVG.as_bytes(), &ImportPolicy::default(), &mut SequentialIdMint::new(1), &token)
            .unwrap_err();
        assert_eq!(err, VectorError::Canceled);
        assert_eq!(err.failure_code(), None);
    });
}

#[test]
fn unit_loss_stale_profile() {
    bounded_case("unit_loss_stale_profile", || {
        // Arc: reported as an approximation and delivered as cubic anchors.
        let arc = r##"<svg xmlns="http://www.w3.org/2000/svg"><path d="M0 0 A 10 10 0 0 1 20 0" fill="none" stroke="#000"/></svg>"##;
        let p = import(arc).unwrap();
        assert!(p.loss.contains(&LossEntry {
            path: "/svg[1]/path[1]".into(),
            kind: LossKind::Approximated,
            code: "arc_to_cubic",
        }));
        let anchors = &p.paths[0].anchors;
        assert!(anchors.len() >= 2 && !p.paths[0].closed);
        assert!(anchors.iter().any(|a| xy(a)[2] != (0.0, 0.0)), "curve handles present");
        // Half circle radius 10 px ends at (20,0) px = (15,0) pt.
        let end = xy(anchors.last().unwrap())[0];
        assert!((end.0 - 15.0).abs() < 1e-9 && end.1.abs() < 1e-9);

        // Gradient fill and filter: reported, never silently dropped.
        let gradient = r##"<svg xmlns="http://www.w3.org/2000/svg"><defs><linearGradient id="g"/></defs><rect width="10" height="10" fill="url(#g)" filter="url(#f)"/></svg>"##;
        let p = import(gradient).unwrap();
        for code in ["unsupported_defs", "unsupported_paint_server", "unsupported_filter"] {
            assert!(p.has_loss_code(code), "missing {code}: {:?}", p.loss);
        }

        // Geometry outside 1e9 or non-finite after the transform is typed InvalidGeometry.
        let huge = r#"<svg xmlns="http://www.w3.org/2000/svg"><path d="M0 0 L2e9 5"/></svg>"#;
        assert_eq!(import(huge).unwrap_err(), VectorError::InvalidGeometry);
        let overflow = r#"<svg xmlns="http://www.w3.org/2000/svg"><path d="M0 0 L10 10" transform="scale(1e308)"/></svg>"#;
        assert_eq!(import(overflow).unwrap_err(), VectorError::InvalidGeometry);

        // Units: width 2in x height 1in = 192 x 96 px = 144 x 72 pt; 25.4mm = 96 px = 72 pt.
        let units = r#"<svg xmlns="http://www.w3.org/2000/svg" width="2in" height="1in"><rect x="25.4mm" y="0" width="10" height="10"/></svg>"#;
        let p = import(units).unwrap();
        assert_eq!(p.provenance.viewport_px, [192.0, 96.0]);
        let artboard = &p.snapshot.document().artboards[0].bounds;
        assert_eq!((artboard.width.value, artboard.height.value), (144.0, 72.0));
        assert!((xy(&p.paths[0].anchors[0])[0].0 - 72.0).abs() < 1e-9);
        // A different ppi is an explicit, recorded policy: 72 ppi makes 1 px = 1 pt.
        let mut policy = ImportPolicy {
            pixels_per_inch: 72.0,
            ..ImportPolicy::default()
        };
        let p = import_with(units, &policy).unwrap();
        assert!((p.provenance.px_to_pt - 1.0).abs() < 1e-12);
        policy.pixels_per_inch = 0.0;
        assert!(matches!(import_with(units, &policy), Err(VectorError::InvalidPolicy(_))));

        // No colour profile is claimed: the proposal stays unassigned, so Folio refuses to treat
        // it as available until a host binds one.
        let p = import(EDIT_SVG).unwrap();
        assert!(p.snapshot.require_available().is_err());

        // PDF is a provider port; without a provider the answer is explicit.
        assert_eq!(
            NoPdfProvider.import_page(b"%PDF-1.7", 0, &CancellationToken::default()).unwrap_err(),
            VectorError::PdfUnavailable
        );
    });
}

#[test]
fn folio_vector_layer_validates() {
    bounded_case("folio_vector_layer_validates", || {
        let p = import(EDIT_SVG).unwrap();
        // Independent re-validation of the proposal's document with a different resolver.
        let doc = p.snapshot.document().clone();
        validate_document(doc.clone(), Budget::default(), &CancellationToken::default(), &Unresolved).unwrap();

        let mut layer_paths = BTreeSet::new();
        for layer in &doc.layers {
            DomainId::parse(&layer.layer_id).unwrap();
            assert_eq!(DomainId::parse(&layer.layer_id).unwrap().prefix(), "SLYR");
            if let Payload::Vector { path_ids } = &layer.payload {
                assert_eq!(layer.kind, "vector");
                for id in path_ids {
                    assert_eq!(DomainId::parse(id).unwrap().prefix(), "SVPT");
                    layer_paths.insert(id.clone());
                }
            }
        }
        let proposal_paths: BTreeSet<_> = p.paths.iter().map(|x| x.path_id.clone()).collect();
        assert_eq!(layer_paths, proposal_paths);
        assert_eq!(doc.layers.iter().filter(|l| l.kind == "group").count(), 1);
        assert!(p.paths.iter().all(|x| doc.layers.iter().any(|l| l.layer_id == x.layer_id)));
        assert_eq!(doc.geometry_unit, hsk_studio_folio::GeometryUnit::Pt);

        // Nib geometry from the anchors: M, C (handles (0,30)), L, close (straight closing segment).
        let g = p.paths[0].to_geometry(&PathLimits::default()).unwrap();
        let pp = PathPoint::new;
        assert_eq!(
            g.elements,
            vec![
                PathElement::MoveTo(pp(7.5, 15.0)),
                PathElement::CubicTo(pp(7.5, 45.0), pp(37.5, 45.0), pp(37.5, 15.0)),
                PathElement::LineTo(pp(67.5, 15.0)),
                PathElement::Close,
            ]
        );
    });
}
