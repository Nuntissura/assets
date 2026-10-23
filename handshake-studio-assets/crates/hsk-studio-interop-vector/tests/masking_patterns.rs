//! Slice 3 contract: pattern paints, clip paths and masks resolve to an exact result (a no-op clip
//! or mask is dropped, an empty one hides the element, a zero-size pattern paints nothing), to a
//! Folio vector record (slice 4, `tests/vector_records_roundtrip.rs`: anything that restricts the
//! element and is representable is stored and attached) or to a typed loss with a specific code.
//! Synthetic SVGs only; coordinates are chosen so each expected result follows from the rectangles
//! in the comments. The viewBox equals the size, so 1 px = 0.75 pt.
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{
    Budget, Isolation, LayerAppearance, ProfileBinding, StudioAppearanceRow, StudioColor,
    StudioLayer, StudioMask, StudioPaint, StudioPaintStyle, Unresolved, validate_document,
};
use hsk_studio_interop_vector::*;
use hsk_studio_nib::Anchor;

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

fn import_with(svg: &str, policy: &ImportPolicy) -> Result<VectorProposal> {
    import_svg(
        svg.as_bytes(),
        policy,
        &mut SequentialIdMint::new(5),
        &CancellationToken::default(),
    )
}

fn import(svg: &str) -> VectorProposal {
    import_with(svg, &ImportPolicy::default()).unwrap()
}

/// 100 x 100 viewport; `defs` goes into `<defs>`, `body` after it.
fn doc(defs: &str, body: &str) -> String {
    format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="100" height="100" viewBox="0 0 100 100"><defs>{defs}</defs>{body}</svg>"##
    )
}

fn has(p: &VectorProposal, kind: LossKind, code: &str) -> bool {
    p.loss.iter().any(|l| l.kind == kind && l.code == code)
}

fn count(p: &VectorProposal, code: &str) -> usize {
    p.loss.iter().filter(|l| l.code == code).count()
}

fn at(p: &VectorProposal, path: &str, kind: LossKind, code: &str) -> bool {
    p.loss
        .iter()
        .any(|l| l.path == path && l.kind == kind && l.code == code)
}

fn layer<'a>(p: &'a VectorProposal, name: &str) -> &'a StudioLayer {
    p.snapshot
        .document()
        .layers
        .iter()
        .find(|l| l.name == name)
        .unwrap_or_else(|| panic!("no layer {name}"))
}

fn style_of<'a>(p: &'a VectorProposal, name: &str) -> Option<&'a StudioPaintStyle> {
    p.layer_style(&layer(p, name).layer_id)
}

fn solid(r: u8, g: u8, b: u8) -> StudioPaint {
    StudioPaint::Solid {
        colour: StudioColor {
            r: f64::from(r) / 255.0,
            g: f64::from(g) / 255.0,
            b: f64::from(b) / 255.0,
            colour_profile_id: ProfileBinding::Unassigned(()),
        },
    }
}

fn fill_of(style: &StudioPaintStyle) -> Option<&StudioPaint> {
    style.rows.iter().find_map(|r| match r {
        StudioAppearanceRow::Fill { paint, .. } => Some(paint),
        StudioAppearanceRow::Stroke { .. } => None,
    })
}

fn stroke_of(style: &StudioPaintStyle) -> Option<&StudioPaint> {
    style.rows.iter().find_map(|r| match r {
        StudioAppearanceRow::Stroke { paint, .. } => Some(paint),
        StudioAppearanceRow::Fill { .. } => None,
    })
}

fn coords(a: &Anchor) -> (f64, f64) {
    (a.position.x.value(), a.position.y.value())
}

fn same_geometry(a: &[VectorPath], b: &[VectorPath]) {
    assert_eq!(a.len(), b.len());
    for (p, q) in a.iter().zip(b) {
        assert_eq!((p.closed, p.winding, p.anchors.len()), (q.closed, q.winding, q.anchors.len()));
        for (x, y) in p.anchors.iter().zip(&q.anchors) {
            let (u, v) = (coords(x), coords(y));
            assert!((u.0 - v.0).abs() < 1e-9 && (u.1 - v.1).abs() < 1e-9, "{u:?} != {v:?}");
        }
    }
}

const SQUARE_CLIP: &str = r##"<clipPath id="c"><rect width="100" height="100"/></clipPath>"##;
const INNER: &str = r##"<rect id="r" x="10" y="10" width="30" height="30"/>"##;

/// SVG 1.1 14.3 and CSS Masking 5.1/6.1: a clip that contains everything the element paints is
/// dropped without loss of appearance, an empty clip hides the element, an invalid reference clips
/// nothing; anything else is a typed loss and the element stays as drawn.
#[test]
fn clip_path_exact_or_typed_loss() {
    bounded_case("clip_path_exact_or_typed_loss", || {
        // Figma-style wrapper: the clip rect is the whole canvas, so it changes nothing.
        let p = import(&doc(SQUARE_CLIP, &format!(r##"<g id="a" clip-path="url(#c)">{INNER}</g>"##)));
        assert!(at(&p, "/svg[1]/g[1]", LossKind::Ignored, "clip_path_noop_dropped"), "{:?}", p.loss);
        assert!(at(&p, "/svg[1]/defs[1]/clipPath[1]", LossKind::Ignored, "clip_path_definition_not_stored"));
        assert!(p.loss.iter().all(|l| l.kind != LossKind::Unsupported), "{:?}", p.loss);
        assert!(layer(&p, "a").visible && layer(&p, "r").visible);
        // Round trip: the export carries no clip, and the reopened file has none to report.
        let text = export_proposal(&p, &ExportOptions::default()).unwrap();
        let q = import(&text);
        same_geometry(&p.paths, &q.paths);
        assert!(!q.loss.iter().any(|l| l.code.contains("clip")), "{:?}", q.loss);

        // Hanging out of the clip (x 90..120 against 0..100): not provable as a no-op, so the clip
        // is stored in the Folio vector records (a hidden group with the clip rectangle, one
        // attachment on the group) and nothing is lost; the element path plus the clip path.
        let p = import(&doc(
            SQUARE_CLIP,
            r##"<g id="a" clip-path="url(#c)"><rect x="90" y="10" width="30" height="30"/></g>"##,
        ));
        assert!(!has(&p, LossKind::Unsupported, "clip_path_not_representable"), "{:?}", p.loss);
        assert!(layer(&p, "a").visible && p.paths.len() == 2);
        assert_eq!((p.vector_records.clips.len(), p.vector_records.attachments.len()), (1, 1));
        assert!(p.vector_records.attachment(&layer(&p, "a").layer_id).is_some_and(|a| a.clip_id.is_some()));

        // The stroke counts: a 10 px miter stroke reaches 20 px past the edge in the worst case
        // (limit 4), so the 0..100 clip does not contain it (stored) and a 25 px margin does.
        let stroked = r##"<rect id="s" width="100" height="100" fill="none" stroke="#000" stroke-width="10" clip-path="url(#X)"/>"##;
        let tight = import(&doc(SQUARE_CLIP, &stroked.replace("#X", "#c")));
        assert!(!has(&tight, LossKind::Unsupported, "clip_path_not_representable"));
        assert_eq!(tight.vector_records.clips.len(), 1);
        let wide = r##"<clipPath id="w"><rect x="-25" y="-25" width="150" height="150"/></clipPath>"##;
        let loose = import(&doc(wide, &stroked.replace("#X", "#w")));
        assert!(has(&loose, LossKind::Ignored, "clip_path_noop_dropped"), "{:?}", loose.loss);

        // Empty clips remove everything (CSS Masking 6.1): no child, a hidden child, a zero-size
        // rect. The layer is hidden, not deleted, and the export says so.
        for empty in [
            r##"<clipPath id="e"/>"##,
            r##"<clipPath id="e"><rect width="100" height="100" display="none"/></clipPath>"##,
            r##"<clipPath id="e"><rect width="100" height="100" style="visibility:hidden"/></clipPath>"##,
            r##"<clipPath id="e"><rect width="0" height="100"/></clipPath>"##,
        ] {
            let p = import(&doc(empty, r##"<rect id="r" x="10" y="10" width="30" height="30" clip-path="url(#e)"/>"##));
            assert!(has(&p, LossKind::Approximated, "clip_path_empty_hides_element"), "{empty}: {:?}", p.loss);
            assert!(!layer(&p, "r").visible && p.paths.len() == 1);
            let text = export_proposal(&p, &ExportOptions::default()).unwrap();
            assert!(text.contains("display=\"none\""), "{text}");
            assert!(!layer(&import(&text), "r").visible);
        }

        // clipPathUnits: objectBoundingBox 0..1 is the element's own box (contains it exactly);
        // half of it does not (stored, converted with the box); a `transform` on the clipPath
        // makes the order undecided (typed).
        let obb = |rect: &str, extra: &str| {
            let defs = format!(r##"<clipPath id="o" clipPathUnits="objectBoundingBox"{extra}>{rect}</clipPath>"##);
            import(&doc(&defs, &format!(r##"<g id="g" clip-path="url(#o)">{INNER}</g>"##)))
        };
        assert!(has(&obb(r##"<rect width="1" height="1"/>"##, ""), LossKind::Ignored, "clip_path_noop_dropped"));
        let half = obb(r##"<rect width="0.5" height="1"/>"##, "");
        assert!(!has(&half, LossKind::Unsupported, "clip_path_not_representable"));
        // The box of INNER is 10..40 px: the stored clip is its left half, 10..25 px = 7.5..18.75 pt.
        let member = &half.vector_records.clips[0].source_layer_id;
        let xs: Vec<f64> = half
            .paths
            .iter()
            .filter(|path| {
                half.snapshot.document().layers.iter().any(|l| {
                    l.layer_id == path.layer_id
                        && matches!(&l.parent, hsk_studio_folio::Parent::Node(n) if &n.node_id == member)
                })
            })
            .flat_map(|path| path.anchors.iter().map(|a| a.position.x.value()))
            .collect();
        assert!(xs.iter().all(|x| (x - 7.5).abs() < 1e-9 || (x - 18.75).abs() < 1e-9), "{xs:?}");
        assert!(has(
            &obb(r##"<rect width="1" height="1"/>"##, r##" transform="translate(0.001 0)""##),
            LossKind::Unsupported,
            "clip_path_not_representable"
        ));

        // Illustrator-style `use` of a rect in defs; its x shifts the region (5..105).
        let defs = r##"<rect id="base" width="100" height="100"/><clipPath id="u"><use xlink:href="#base" x="5"/></clipPath>"##;
        let p = import(&doc(defs, r##"<rect id="r" x="10" y="10" width="30" height="30" clip-path="url(#u)"/>"##));
        assert!(has(&p, LossKind::Ignored, "clip_path_noop_dropped"), "{:?}", p.loss);
        let p = import(&doc(defs, r##"<rect id="r" x="0" y="10" width="30" height="30" clip-path="url(#u)"/>"##));
        assert!(!has(&p, LossKind::Unsupported, "clip_path_not_representable"), "{:?}", p.loss);
        assert_eq!(p.vector_records.clips.len(), 1, "the `use` member is stored with its shift");

        // Nested clip-path: the intersection must not restrict either (50x50 inner clip); when it
        // does, both are stored (the nested one as a clip of the member or of the clipPath).
        let nested = r##"<clipPath id="n3"><rect width="50" height="50"/></clipPath>
            <clipPath id="n2" clip-path="url(#n3)"><rect width="100" height="100"/></clipPath>
            <clipPath id="n4"><rect width="100" height="100" clip-path="url(#n3)"/></clipPath>
            <clipPath id="n5" clip-path="url(#nope)"><rect width="100" height="100"/></clipPath>"##;
        for (id, w, free) in [("n2", 30, true), ("n2", 60, false), ("n4", 30, true), ("n4", 60, false), ("n5", 60, true)] {
            let body = format!(r##"<rect x="10" y="10" width="{w}" height="30" clip-path="url(#{id})"/>"##);
            let p = import(&doc(nested, &body));
            if free {
                assert!(has(&p, LossKind::Ignored, "clip_path_noop_dropped"), "{id} width {w}: {:?}", p.loss);
                assert!(p.vector_records.clips.is_empty());
            } else {
                assert!(!has(&p, LossKind::Unsupported, "clip_path_not_representable"), "{id} width {w}: {:?}", p.loss);
                // The clipPath and the intersecting clip are two records, each attached once.
                assert_eq!(p.vector_records.clips.len(), 2, "{id} width {w}: {:?}", p.loss);
                assert_eq!(p.vector_records.attachments.len(), 2, "{id} width {w}");
            }
        }

        // clip-rule only matters for shapes that are not analysed; it is stored with them (it was
        // a typed loss before the vector records existed).
        let tri = |rule: &str| {
            let defs = format!(r##"<clipPath id="p"><path d="M0 0L10 0L10 10Z" {rule}/></clipPath>"##);
            import(&doc(&defs, r##"<rect x="1" y="1" width="2" height="2" clip-path="url(#p)"/>"##))
        };
        for (rule, want) in [
            (r##"clip-rule="evenodd""##, hsk_studio_folio::StudioClipRule::EvenOdd),
            (r##"clip-rule="nonzero""##, hsk_studio_folio::StudioClipRule::NonZero),
        ] {
            let p = tri(rule);
            assert!(!has(&p, LossKind::Unsupported, "clip_rule_evenodd"));
            assert_eq!(p.vector_records.clips.len(), 1, "{rule}: {:?}", p.loss);
            assert_eq!(p.vector_records.clips[0].clip_rule, want);
        }

        // Invalid references clip nothing; a basic shape and a file are reported, never followed.
        for (value, kind, code) in [
            ("url(#nope)", LossKind::Ignored, "clip_path_reference_invalid"),
            ("url(#r0)", LossKind::Ignored, "clip_path_reference_invalid"),
            ("circle(40%)", LossKind::Unsupported, "unsupported_clip_path"),
            ("url(file.svg#c)", LossKind::ExternalDenied, "external_clip_path_denied"),
        ] {
            let body = format!(r##"<rect id="r0" width="5" height="5" clip-path="{value}"/>"##);
            let p = import(&doc(SQUARE_CLIP, &body));
            assert!(has(&p, kind, code), "{value}: {:?}", p.loss);
            assert!(layer(&p, "r0").visible);
        }

        // The Folio mask, clip-to-base flag, blend mode and fill opacity have no SVG counterpart:
        // the export reports each instead of dropping it.
        let mut p = import(&doc("", INNER));
        let mut document = p.snapshot.document().clone();
        let vector = document.layers.iter_mut().find(|l| l.kind == "vector").unwrap();
        vector.appearance = Some(LayerAppearance {
            opacity: 100.0,
            fill_opacity: 50.0,
            blend_mode: 5,
            clipped: true,
            isolation: Isolation::Isolated,
            mask: Some(StudioMask { tiles: Vec::new(), enabled: true, linked: true }),
        });
        p.snapshot = validate_document(document.clone(), Budget::default(), &CancellationToken::default(), &Unresolved).unwrap();
        let out = export_proposal_with_loss(&p, &ExportOptions::default()).unwrap();
        for code in [
            "layer_mask_not_exported",
            "layer_clipped_to_base_not_exported",
            "layer_blend_mode_not_exported",
            "layer_fill_opacity_not_exported",
        ] {
            assert!(out.loss.iter().any(|l| l.code == code && l.kind == LossKind::Unsupported), "{code}: {:?}", out.loss);
        }
        let vector = document.layers.iter_mut().find(|l| l.kind == "vector").unwrap();
        vector.appearance = Some(LayerAppearance {
            mask: Some(StudioMask { tiles: Vec::new(), enabled: false, linked: true }),
            ..LayerAppearance::identity(false)
        });
        p.snapshot = validate_document(document, Budget::default(), &CancellationToken::default(), &Unresolved).unwrap();
        let out = export_proposal_with_loss(&p, &ExportOptions::default()).unwrap();
        assert!(out.loss.iter().any(|l| l.code == "layer_mask_disabled_not_exported" && l.kind == LossKind::Ignored));
        assert!(!out.loss.iter().any(|l| l.code == "layer_mask_not_exported"));
    });
}

/// SVG 1.1 14.4, CSS Masking 7.1 and 9.2: a mask whose region and topmost opaque rectangle cover
/// the element is the identity (white for `mask-type: luminance`, any colour for `alpha`); an
/// empty mask, a zero region or an invalid reference hides the element; everything else is a typed
/// loss that names the mask type.
#[test]
fn mask_exact_or_typed_loss() {
    bounded_case("mask_exact_or_typed_loss", || {
        let masked = |defs: &str| import(&doc(defs, &format!(r##"<g id="a" mask="url(#m)">{INNER}</g>"##)));
        let no_op = |p: &VectorProposal| at(p, "/svg[1]/g[1]", LossKind::Ignored, "mask_noop_dropped");
        let open = |p: &VectorProposal| at(p, "/svg[1]/g[1]", LossKind::Unsupported, "mask_not_representable");
        // A mask that restricts the element is stored in the vector records (one record, one
        // attachment on the group) and is no longer a loss.
        let stored = |p: &VectorProposal| {
            p.vector_records.masks.len() == 1
                && p.vector_records.attachments.len() == 1
                && !p.loss.iter().any(|l| l.code == "mask_not_representable")
        };
        let user = r##"maskUnits="userSpaceOnUse" x="0" y="0" width="100" height="100""##;

        // Figma style: alpha mask, grey rect over the whole canvas -> value 1 everywhere.
        let p = masked(&format!(r##"<mask id="m" style="mask-type:alpha" {user}><rect width="100" height="100" fill="#D9D9D9"/></mask>"##));
        assert!(no_op(&p), "{:?}", p.loss);
        assert!(at(&p, "/svg[1]/defs[1]/mask[1]", LossKind::Ignored, "mask_definition_not_stored"));
        assert!(p.loss.iter().all(|l| l.kind != LossKind::Unsupported), "{:?}", p.loss);
        // The same grey is luminance 0.85 for the default type: a real mask, stored with its mode.
        let p = masked(&format!(r##"<mask id="m" {user}><rect width="100" height="100" fill="#D9D9D9"/></mask>"##));
        assert!(stored(&p), "{:?}", p.loss);
        assert_eq!(p.vector_records.masks[0].mode, hsk_studio_folio::StudioMaskMode::Luminance);
        assert!(p.loss.iter().all(|l| l.kind != LossKind::Unsupported), "{:?}", p.loss);
        // White is 1 in either type; the property name and keyword are case-insensitive CSS.
        for white in ["white", "#FFFFFF", "rgb(255,255,255)"] {
            let p = masked(&format!(r##"<mask id="m" {user}><rect width="100" height="100" fill="{white}"/></mask>"##));
            assert!(no_op(&p), "{white}: {:?}", p.loss);
        }
        for ty in [r##"mask-type="alpha""##, r##"style="MASK-TYPE:Alpha""##] {
            let p = masked(&format!(r##"<mask id="m" {ty} {user}><rect width="100" height="100" fill="#808080"/></mask>"##));
            assert!(no_op(&p), "{ty}: {:?}", p.loss);
        }
        // An unusable mask-type is not guessed: typed, not stored, no type code.
        let p = masked(&format!(r##"<mask id="m" mask-type="bogus" {user}><rect width="100" height="100" fill="white"/></mask>"##));
        assert!(open(&p) && !has(&p, LossKind::Unsupported, "mask_type_alpha") && !has(&p, LossKind::Unsupported, "mask_type_luminance"));
        assert!(p.vector_records.masks.is_empty() && has(&p, LossKind::Unsupported, "mask_type_unusable"));

        // Default region -10%/-10%/120%/120% of the box (30 px -> 3 px margin): contains the
        // element; a stroke reaches further (8 px * 0.5 * 4) and a userSpaceOnUse region can
        // start after the element: both stored.
        let white = r##"<rect width="100" height="100" fill="white"/>"##;
        assert!(no_op(&masked(&format!(r##"<mask id="m">{white}</mask>"##))));
        let stroked = import(&doc(
            &format!(r##"<mask id="m">{white}</mask>"##),
            r##"<g id="a" mask="url(#m)"><rect x="10" y="10" width="30" height="30" fill="none" stroke="#000" stroke-width="8"/></g>"##,
        ));
        assert!(stored(&stroked), "{:?}", stroked.loss);
        let late = masked(&format!(r##"<mask id="m" maskUnits="userSpaceOnUse" x="20" y="0" width="80" height="100">{white}</mask>"##));
        assert!(stored(&late), "{:?}", late.loss);

        // maskContentUnits=objectBoundingBox: 0..1 is the element's box; half of it is not enough.
        assert!(no_op(&masked(r##"<mask id="m" maskContentUnits="objectBoundingBox"><rect width="1" height="1" fill="white"/></mask>"##)));
        assert!(stored(&masked(r##"<mask id="m" maskContentUnits="objectBoundingBox"><rect width="0.5" height="1" fill="white"/></mask>"##)));

        // Only the topmost child decides; it must be opaque, unstroked and unfiltered.
        let small = r##"<rect width="5" height="5" fill="white"/>"##;
        assert!(no_op(&masked(&format!(r##"<mask id="m" {user}>{small}{white}</mask>"##))));
        assert!(stored(&masked(&format!(r##"<mask id="m" {user}>{white}{small}</mask>"##))));
        for top in [
            r##"<rect width="100" height="100" fill="white" fill-opacity="0.5"/>"##,
            r##"<rect width="100" height="100" fill="white" opacity="0.99"/>"##,
            r##"<rect width="100" height="100" fill="white" stroke="black"/>"##,
            r##"<rect width="100" height="100" fill="white" mask="url(#m)"/>"##,
            r##"<rect width="100" height="100" fill="white" filter="url(#f)"/>"##,
            r##"<rect width="100" height="100" rx="5" fill="white"/>"##,
            r##"<path d="M0 0H100V100H0Z" fill="white"/>"##,
            r##"<rect width="100" height="100" fill="url(#g)"/>"##,
        ] {
            // Stored, with the content's own losses reported where they are (a mask that uses
            // itself is a reported loop for the inner use only).
            let p = masked(&format!(r##"<mask id="m" {user}>{top}</mask>"##));
            assert!(
                p.vector_records.masks.len() == 1 && p.vector_records.attachments.len() == 1,
                "{top}: {:?}",
                p.loss
            );
        }
        // A reference on the mask element itself is not stored.
        assert!(open(&masked(&format!(r##"<mask id="m" mask="url(#m)" {user}>{white}</mask>"##))));

        // Exact hides: zero region, no content, hidden content, invalid reference (transparent
        // black, CSS Masking 7.1).
        for defs in [
            format!(r##"<mask id="m" maskUnits="userSpaceOnUse" x="0" y="0" width="0" height="100">{white}</mask>"##),
            r##"<mask id="m"/>"##.to_owned(),
            format!(r##"<mask id="m" {user}><rect width="100" height="100" display="none"/></mask>"##),
        ] {
            let p = masked(&defs);
            assert!(at(&p, "/svg[1]/g[1]", LossKind::Approximated, "mask_empty_hides_element"), "{defs}: {:?}", p.loss);
            assert!(!layer(&p, "a").visible);
        }
        let p = import(&doc("", &format!(r##"<g id="a" mask="url(#nope)">{INNER}</g>"##)));
        assert!(has(&p, LossKind::Approximated, "mask_reference_invalid_hidden") && !layer(&p, "a").visible);

        // A file is never followed (the element stays); unparsable values are typed.
        for (value, kind, code) in [
            ("url(http://example.invalid/m.svg#a)", LossKind::ExternalDenied, "external_mask_denied"),
            ("url(#m) alpha", LossKind::Unsupported, "mask_value_unparsed"),
            ("linear-gradient(red, blue)", LossKind::Unsupported, "unsupported_mask"),
        ] {
            let p = import(&doc("", &format!(r##"<g id="a" mask="{value}">{INNER}</g>"##)));
            assert!(has(&p, kind, code), "{value}: {:?}", p.loss);
            assert!(layer(&p, "a").visible);
        }

        // Huge region numbers overflow to infinity inside the comparison; no panic, no claim.
        let p = masked(r##"<mask id="m" maskUnits="userSpaceOnUse" x="1e308" y="0" width="1e308" height="100"><rect width="100" height="100" fill="white"/></mask>"##);
        assert!(open(&p), "{:?}", p.loss);
    });
}

/// SVG 1.1 13.3 and SVG 2 16.1: a pattern paints nothing for a zero or missing tile, a singular
/// `patternTransform` or no content (exact); an `href` chain is followed with a visited set, depth
/// cap and `href` over `xlink:href`; a real tiling is a typed loss and the author's fallback
/// colour, if any, stands in for it (reported).
#[test]
fn pattern_paint_exact_or_typed_loss() {
    bounded_case("pattern_paint_exact_or_typed_loss", || {
        let defs = r##"
  <pattern id="base" width="10" height="10" patternUnits="userSpaceOnUse"><circle cx="5" cy="5" r="2"/></pattern>
  <pattern id="child" xlink:href="#base"/>
  <pattern id="zero" patternUnits="userSpaceOnUse"><circle r="2"/></pattern>
  <pattern id="empty" width="5" height="5"><title>no content</title></pattern>
  <pattern id="ca" href="#cb" width="4" height="4"/>
  <pattern id="cb" href="#ca"><rect width="1" height="1"/></pattern>
  <pattern id="both" href="#base" xlink:href="#ca"/>
  <pattern id="neg" width="-5" height="5"><rect width="1" height="1"/></pattern>
  <pattern id="sing" width="4" height="4" patternTransform="scale(0)"><rect width="1" height="1"/></pattern>
  <pattern id="units" width="4" height="4" patternUnits="bogus"><rect width="1" height="1"/></pattern>"##;
        let body = r##"
  <rect id="r_base" width="10" height="10" fill="url(#base)"/>
  <rect id="r_child" width="10" height="10" fill="url(#child) #00ff00"/>
  <rect id="r_zero" width="10" height="10" fill="url(#zero)"/>
  <rect id="r_empty" width="10" height="10" fill="url(#empty)"/>
  <rect id="r_cyc" width="10" height="10" fill="url(#ca) #112233"/>
  <rect id="r_both" width="10" height="10" fill="url(#both)"/>
  <rect id="r_neg" width="10" height="10" fill="url(#neg) #445566"/>
  <rect id="r_sing" width="10" height="10" fill="url(#sing)"/>
  <rect id="r_stroke" width="10" height="10" fill="none" stroke="url(#base)" stroke-width="2"/>
  <rect id="r_units" width="10" height="10" fill="url(#units)"/>
  <rect id="r_missing" width="10" height="10" fill="url(#nowhere) #778899"/>"##;
        let p = import(&doc(defs, body));

        // Real tilings (base, through xlink:href, through href over a cyclic xlink:href, stroke)
        // are stored in the vector records; only the unparsable `patternUnits` is a loss. A tiling
        // without a fallback colour stands in with the first solid colour of its tile (the
        // default black fill of the circle here), reported.
        assert_eq!(count(&p, "pattern_paint_not_representable"), 1, "{:?}", p.loss);
        assert_eq!(p.vector_records.patterns.len(), 3, "base, child, both: {:?}", p.loss);
        assert_eq!(p.vector_records.pattern_paints.len(), 4, "fills of base, child, both and the stroke");
        for name in ["r_base", "r_both"] {
            assert_eq!(fill_of(style_of(&p, name).unwrap()), Some(&solid(0, 0, 0)), "{name}");
        }
        assert_eq!(count(&p, "pattern_stand_in_derived"), 3);
        assert_eq!(fill_of(style_of(&p, "r_child").unwrap()), Some(&solid(0, 255, 0)));
        assert!(has(&p, LossKind::Approximated, "pattern_fallback_color_used"));
        assert_eq!(count(&p, "pattern_fallback_color_used"), 1);
        assert_eq!(stroke_of(style_of(&p, "r_stroke").unwrap()), Some(&solid(0, 0, 0)));
        assert!(style_of(&p, "r_units").is_none());
        // Exact "paints nothing" results: no stand-in, no representability loss.
        assert!(has(&p, LossKind::Ignored, "pattern_zero_size_no_paint"));
        assert!(has(&p, LossKind::Ignored, "pattern_empty_no_paint"));
        assert!(has(&p, LossKind::Ignored, "pattern_transform_singular_no_paint"));
        for name in ["r_zero", "r_empty", "r_sing"] {
            assert!(style_of(&p, name).is_none(), "{name}");
        }
        // The href loop is invalid (SVG 2 16.1.2): fallback colour, one loop report (the `both`
        // pattern follows `href`, not the looping `xlink:href`).
        assert_eq!(count(&p, "pattern_href_cycle"), 1, "{:?}", p.loss);
        assert_eq!(fill_of(style_of(&p, "r_cyc").unwrap()), Some(&solid(0x11, 0x22, 0x33)));
        // A negative tile is an error: invalid reference, fallback.
        assert!(has(&p, LossKind::Unsupported, "pattern_size_negative"));
        assert_eq!(fill_of(style_of(&p, "r_neg").unwrap()), Some(&solid(0x44, 0x55, 0x66)));
        assert!(has(&p, LossKind::Unsupported, "pattern_units_unparsed"));
        // A missing id is not a pattern: the generic paint server loss and the fallback.
        assert!(has(&p, LossKind::Unsupported, "unsupported_paint_server"));
        assert_eq!(fill_of(style_of(&p, "r_missing").unwrap()), Some(&solid(0x77, 0x88, 0x99)));
        // A definition that no use stored is reported once: the seven that were zero, empty,
        // looping, negative, singular, with unparsable units or never reached.
        assert_eq!(count(&p, "pattern_definition_not_stored"), 7, "{:?}", p.loss);
        assert!(p.design.paint_styles.iter().all(|s| s.rows.iter().all(|r| match r {
            StudioAppearanceRow::Fill { paint, .. } | StudioAppearanceRow::Stroke { paint, .. } =>
                matches!(paint, StudioPaint::Solid { .. }),
        })));

        // Depth cap: a chain longer than 8 templates is refused with a typed loss, here and for
        // gradients (whose chain used to be cut silently).
        let chain = |tag: &str, last: &str| -> String {
            (0..12)
                .map(|i| {
                    if i == 11 {
                        format!(r##"<{tag} id="t11"{last}"##)
                    } else {
                        format!(r##"<{tag} id="t{i}" href="#t{}"/>"##, i + 1)
                    }
                })
                .collect()
        };
        let long = import(&doc(
            &chain("pattern", r##" width="4" height="4"><rect width="1" height="1"/></pattern>"##),
            r##"<rect width="9" height="9" fill="url(#t0)"/>"##,
        ));
        assert!(has(&long, LossKind::Unsupported, "pattern_href_chain_too_long"), "{:?}", long.loss);
        let long = import(&doc(
            &chain("linearGradient", r##"><stop offset="0" stop-color="red"/><stop offset="1" stop-color="blue"/></linearGradient>"##),
            r##"<rect width="9" height="9" fill="url(#t0)"/>"##,
        ));
        assert!(has(&long, LossKind::Unsupported, "gradient_href_chain_too_long"), "{:?}", long.loss);
        // Within the cap the chain is followed to its content (7 hops).
        let short = import(&doc(
            &(0..8)
                .map(|i| if i == 7 { r##"<pattern id="t7" width="4" height="4"><rect width="1" height="1"/></pattern>"##.to_owned() } else { format!(r##"<pattern id="t{i}" href="#t{}"/>"##, i + 1) })
                .collect::<String>(),
            r##"<rect width="9" height="9" fill="url(#t0)"/>"##,
        ));
        assert!(!has(&short, LossKind::Unsupported, "pattern_paint_not_representable"), "{:?}", short.loss);
        assert_eq!(short.vector_records.patterns.len(), 1);
        assert!(!has(&short, LossKind::Unsupported, "pattern_href_chain_too_long"));
    });
}

/// Hostile reference structures stay bounded: nesting depth, loops, exponential fan-out (one work
/// budget for the whole import), pattern chains, absurd numbers.
#[test]
fn hostile_references_are_bounded() {
    bounded_case("hostile_references_are_bounded", || {
        let content = r##"<rect x="10" y="10" width="30" height="30" clip-path="url(#c0)"/>"##;

        // Depth bomb: 40 clipPaths, each child clipped by the next. Evaluation stops at depth 8.
        let bomb: String = (0..40)
            .map(|i| {
                if i == 39 {
                    r##"<clipPath id="c39"><rect width="100" height="100"/></clipPath>"##.to_owned()
                } else {
                    format!(r##"<clipPath id="c{i}"><rect width="100" height="100" clip-path="url(#c{})"/></clipPath>"##, i + 1)
                }
            })
            .collect();
        let p = import(&doc(&bomb, content));
        assert!(has(&p, LossKind::Unsupported, "clip_path_depth_exceeded"), "{:?}", p.loss);
        assert!(layer(&p, "rect").visible);

        // Loops: two clipPaths clipping each other's child, and a clipPath clipping itself.
        let looped = r##"<clipPath id="c0"><rect width="100" height="100" clip-path="url(#c1)"/></clipPath>
            <clipPath id="c1"><rect width="100" height="100" clip-path="url(#c0)"/></clipPath>"##;
        assert!(has(&import(&doc(looped, content)), LossKind::Unsupported, "clip_path_reference_cycle"));
        let selfish = r##"<clipPath id="c0" clip-path="url(#c0)"><rect width="100" height="100"/></clipPath>"##;
        assert!(has(&import(&doc(selfish, content)), LossKind::Unsupported, "clip_path_reference_cycle"));

        // Fan-out: 5 levels of 8 rectangles that each contain the element and clip it by the next
        // level; the last level contains nothing. About 8^4 evaluations of 49 work units.
        let mut fan = String::new();
        for level in 0..5 {
            fan.push_str(&format!(r##"<clipPath id="c{level}">"##));
            for _ in 0..8 {
                if level == 4 {
                    fan.push_str(r##"<rect width="1" height="1"/>"##);
                } else {
                    fan.push_str(&format!(r##"<rect width="100" height="100" clip-path="url(#c{})"/>"##, level + 1));
                }
            }
            fan.push_str("</clipPath>");
        }
        let svg = doc(&fan, content);
        let mut small = ImportPolicy::default();
        small.limits.max_nodes = 400;
        small.limits.max_segments = 1_000;
        assert_eq!(import_with(&svg, &small).unwrap_err(), VectorError::BudgetExceeded("reference_work"));
        // The default budget (32 units per node and segment of the limits) lets the same file
        // through: the cap is the budget, not a refusal of the structure. The five clipPaths are
        // stored once each (identical uses share a record, so the fan-out of 8 per level does
        // not multiply the records).
        let p = import(&svg);
        assert!(!has(&p, LossKind::Unsupported, "clip_path_not_representable"), "{:?}", p.loss);
        assert_eq!(p.vector_records.clips.len(), 5, "{:?}", p.loss);

        // Many children are examined only up to a cap: 70 non-containing rects ahead of the one
        // that contains are not provable, never an error.
        let wide: String = (0..70)
            .map(|_| r##"<rect width="1" height="1"/>"##)
            .chain([r##"<rect width="100" height="100"/>"##])
            .collect();
        let p = import(&doc(&format!(r##"<clipPath id="c0">{wide}</clipPath>"##), content));
        assert!(has(&p, LossKind::Unsupported, "clip_path_not_representable"));

        // Numbers: a clip rect and a transform at the edge of f64 neither panic nor claim.
        let huge = r##"<clipPath id="c0" transform="scale(1e308)"><rect width="1e308" height="1e308"/></clipPath>"##;
        let p = import(&doc(huge, content));
        assert!(has(&p, LossKind::Unsupported, "clip_path_not_representable"), "{:?}", p.loss);
        let wide_rect = r##"<clipPath id="c0"><rect x="-1e308" y="-1e308" width="2e308" height="1e308"/></clipPath>"##;
        let p = import(&doc(wide_rect, content));
        assert!(has(&p, LossKind::Unsupported, "clip_path_not_representable"));
        // A finite rect whose far edge overflows to infinity must not turn the tolerance infinite
        // and accept everything (this exact case was a false no-op before the finite guard).
        let overflow_rect = r##"<clipPath id="c0"><rect x="1e308" y="0" width="1e308" height="100"/></clipPath>"##;
        let p = import(&doc(overflow_rect, content));
        assert!(has(&p, LossKind::Unsupported, "clip_path_not_representable"), "{:?}", p.loss);
        assert!(!has(&p, LossKind::Ignored, "clip_path_noop_dropped"));
    });
}

/// CSS in `style` and in paint/clip values is tokenised, not matched as text: escapes decode,
/// comments hide declarations, case and whitespace do not matter, `;` inside strings and `url()`
/// does not split, and an escaped delimiter is not a delimiter.
#[test]
fn css_values_are_tokenised_not_substring_matched() {
    bounded_case("css_values_are_tokenised_not_substring_matched", || {
        let gradient = r##"<linearGradient id="g" x1="0" x2="1"><stop offset="0" stop-color="red"/><stop offset="1" stop-color="blue"/></linearGradient>"##;
        let rect = |id: &str, attrs: &str| format!(r##"<rect id="{id}" x="10" y="10" width="30" height="30" {attrs}/>"##);
        let body = [
            // 1 escape: `cl\69p-path:u\72l(#c)` is `clip-path:url(#c)`.
            rect("e1", r##"style="cl\69p-path:u\72l(#c)""##),
            // 2 comment decoy: the commented declaration is not one, the real fill still applies.
            rect("e2", r##"style="/* clip-path:url(#c); */ fill:#ff0000""##),
            // 3 case and whitespace, quoted url.
            rect("e3", r##"style="CLIP-PATH : URL( '#c' ) ;""##),
            // 4 `;` inside a url string does not split; the stroke after it applies; file paint denied.
            rect("e4", r##"style="fill:url('data:image/png;base64,AAAA');stroke:#0000ff;stroke-width:2""##),
            // 5 !important, any case.
            rect("e5", r##"style="fill:#00ff00 !IMPORTANT""##),
            // 6 uppercase function in a presentation attribute.
            rect("e6", r##"fill="URL(#g)""##),
            // 7 an escaped parenthesis is part of an identifier, not a function.
            rect("e7", r##"style="fill:\28red""##),
            // 8 unterminated url, and text after the closing parenthesis.
            rect("e8", r##"style="clip-path:url(#c""##),
            rect("e9", r##"style="clip-path:url(#c)extra""##),
            // 10 a file reference is denied (attribute form).
            rect("e10", r##"clip-path="url(file.svg#c)""##),
            // 11 hex escape swallowing one space inside a bare url token: `\23 c` is `#c`.
            rect("e11", r##"style="clip-path:url(\23 c)""##),
            // 12 comments around a value.
            rect("e12", r##"style="clip-path:/**/url(#c)/* x */""##),
        ]
        .concat();
        let p = import(&doc(&format!("{SQUARE_CLIP}{gradient}"), &body));
        let noop = |n: usize| at(&p, &format!("/svg[1]/rect[{n}]"), LossKind::Ignored, "clip_path_noop_dropped");
        let entry = |n: usize, kind, code| at(&p, &format!("/svg[1]/rect[{n}]"), kind, code);

        assert!(noop(1) && noop(3) && noop(11) && noop(12), "{:?}", p.loss);
        // The decoy comment hid the declaration and kept the next one.
        assert!(!p.loss.iter().any(|l| l.path == "/svg[1]/rect[2]" && l.code.contains("clip")), "{:?}", p.loss);
        assert_eq!(fill_of(style_of(&p, "e2").unwrap()), Some(&solid(255, 0, 0)));
        // The `;` in the url did not eat the stroke.
        assert!(entry(4, LossKind::ExternalDenied, "external_paint_denied"));
        assert_eq!(stroke_of(style_of(&p, "e4").unwrap()), Some(&solid(0, 0, 255)));
        assert_eq!(fill_of(style_of(&p, "e5").unwrap()), Some(&solid(0, 255, 0)));
        assert!(matches!(
            fill_of(style_of(&p, "e6").unwrap()),
            Some(StudioPaint::Gradient { .. })
        ));
        assert!(entry(7, LossKind::Unsupported, "paint_unparsed"));
        assert!(entry(8, LossKind::Unsupported, "clip_path_value_unparsed"));
        assert!(entry(9, LossKind::Unsupported, "clip_path_value_unparsed"));
        assert!(entry(10, LossKind::ExternalDenied, "external_clip_path_denied"));
        // Nothing was fetched or followed: every layer is still there and visible.
        assert_eq!(p.paths.len(), 12);
        assert!((1..=12).all(|n| layer(&p, &format!("e{n}")).visible));
    });
}
