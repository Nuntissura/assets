//! Slice 4 contract: patterns, clip paths and masks are stored in the Folio vector records
//! (`hsk.studio.vector_records@1`) and written back to SVG from them. Synthetic SVGs only, sized
//! so the numbers follow from the rectangles in the comments: the viewBox equals the size, so
//! 1 px = 0.75 pt on import and 1 pt = 4/3 px on export.
//!
//! Round trip means import -> Folio validation -> export -> re-import: the re-imported proposal
//! has the same `signature` (geometry, rules, modes, regions and placements rounded to 1e-6; ids,
//! record counts and the names of hidden definition layers do not take part) and a second
//! export/import cycle changes nothing.
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{
    Budget, StudioAppearanceRow, StudioClipRule, StudioLayer, StudioMaskMode, StudioPaint,
    StudioPatternPlacement, StudioPatternSource, validate_vector_records,
};
use hsk_studio_interop_vector::*;

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
        &mut SequentialIdMint::new(9),
        &CancellationToken::default(),
    )
}

fn import(svg: &str) -> VectorProposal {
    import_with(svg, &ImportPolicy::default()).unwrap()
}

/// A 100 x 100 document (or `size` when given) holding `defs` and `body`.
fn doc_sized(size: u32, defs: &str, body: &str) -> String {
    format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="{size}" height="{size}" viewBox="0 0 {size} {size}"><defs>{defs}</defs>{body}</svg>"##
    )
}

fn doc(defs: &str, body: &str) -> String {
    doc_sized(100, defs, body)
}

fn count(p: &VectorProposal, code: &str) -> usize {
    p.loss.iter().filter(|l| l.code == code).count()
}

fn layer<'a>(p: &'a VectorProposal, name: &str) -> &'a StudioLayer {
    p.snapshot
        .document()
        .layers
        .iter()
        .find(|l| l.name == name)
        .unwrap_or_else(|| panic!("no layer {name}"))
}

fn layer_by_id<'a>(p: &'a VectorProposal, id: &str) -> &'a StudioLayer {
    p.snapshot
        .document()
        .layers
        .iter()
        .find(|l| l.layer_id == id)
        .unwrap_or_else(|| panic!("no layer id {id}"))
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-6
}

fn validate(p: &VectorProposal) {
    validate_vector_records(
        &p.vector_records,
        p.snapshot.document(),
        Some(&p.design),
        Budget::default(),
        &CancellationToken::default(),
    )
    .unwrap_or_else(|e| panic!("Folio refused the vector records: {e:?}"));
}

fn rounded(v: f64) -> String {
    format!("{:.6}", (v * 1e6).round() / 1e6 + 0.0)
}

/// Path geometry of `root` and everything below it, in document order, without any name or id.
fn geometry_below(p: &VectorProposal, root: &str) -> Vec<String> {
    let doc = p.snapshot.document();
    let mut out = Vec::new();
    let mut stack = vec![root.to_owned()];
    while let Some(id) = stack.pop() {
        let l = layer_by_id(p, &id);
        if let hsk_studio_folio::Payload::Vector { path_ids } = &l.payload {
            for path_id in path_ids {
                let path = p.paths.iter().find(|q| &q.path_id == path_id).unwrap();
                let points: Vec<String> = path
                    .anchors
                    .iter()
                    .map(|a| {
                        format!(
                            "{},{}",
                            rounded(a.position.x.value()),
                            rounded(a.position.y.value())
                        )
                    })
                    .collect();
                out.push(format!(
                    "{:?}/{}/{}",
                    path.winding,
                    path.closed,
                    points.join(" ")
                ));
            }
        }
        for c in doc
            .layers
            .iter()
            .filter(|c| matches!(&c.parent, hsk_studio_folio::Parent::Node(n) if n.node_id == id))
        {
            stack.push(c.layer_id.clone());
        }
    }
    out
}

fn effectively_visible(p: &VectorProposal, l: &StudioLayer) -> bool {
    let doc = p.snapshot.document();
    let mut cur = l;
    loop {
        if !cur.visible {
            return false;
        }
        match &cur.parent {
            hsk_studio_folio::Parent::Node(n)
                if n.node_kind == hsk_studio_folio::NodeKind::Layer =>
            {
                cur = doc.layers.iter().find(|x| x.layer_id == n.node_id).unwrap();
            }
            _ => return true,
        }
    }
}

/// What a round trip must preserve (see the module note).
fn signature(p: &VectorProposal) -> Vec<String> {
    let r = &p.vector_records;
    let mut out = Vec::new();
    for pp in &r.pattern_paints {
        let pattern = r.pattern(&pp.pattern_id).unwrap();
        let StudioPatternSource::Definition {
            content_layer_id,
            tile_width,
            tile_height,
            ..
        } = &pattern.source
        else {
            panic!("live source pattern");
        };
        let StudioPatternPlacement::Matrix { matrix } = &pp.placement else {
            panic!("parametric placement");
        };
        out.push(format!(
            "pattern row={} tile={}x{} matrix={:?} art={:?}",
            pp.row,
            rounded(tile_width.value),
            rounded(tile_height.value),
            matrix.map(rounded),
            geometry_below(p, content_layer_id)
        ));
    }
    for c in &r.clips {
        out.push(format!(
            "clip rule={:?} geometry={:?}",
            c.clip_rule,
            geometry_below(p, &c.source_layer_id)
        ));
    }
    for m in &r.masks {
        let region = m
            .region
            .as_ref()
            .map(|b| [&b.x, &b.y, &b.width, &b.height].map(|l| rounded(l.value)));
        out.push(format!(
            "mask mode={:?} clip={} invert={} region={:?} geometry={:?}",
            m.mode,
            m.mask_clip,
            m.invert_mask,
            region,
            geometry_below(p, &m.source_layer_id)
        ));
    }
    for a in &r.attachments {
        let l = layer_by_id(p, &a.layer_id);
        if effectively_visible(p, l) {
            out.push(format!(
                "attach {} clip={} mask={}",
                l.name,
                a.clip_id.is_some(),
                a.mask_id.is_some()
            ));
        }
    }
    out.sort();
    out
}

/// Export, check that the export reported nothing about the records, re-import, and compare.
fn round_trip(p: &VectorProposal) -> VectorProposal {
    validate(p);
    let opts = ExportOptions::default();
    let first = export_proposal_with_loss(p, &opts).unwrap();
    assert!(
        first
            .loss
            .iter()
            .all(|l| !l.code.ends_with("_not_exported")),
        "{:?}",
        first.loss
    );
    let q = import(&first.text);
    validate(&q);
    assert_eq!(signature(p), signature(&q), "{}", first.text);
    // A second cycle is a fixed point.
    let again = import(&export_proposal(&q, &opts).unwrap());
    assert_eq!(signature(&q), signature(&again));
    q
}

/// SVG 1.1 13.3: a tiling pattern fill is a `StudioPattern` plus a binding of the painted style
/// row, with the placement matrix tile space -> document points; the author's fallback colour is
/// the stand-in paint; content leaving the tile is clipped by a tile-cell clip; shared tiles share
/// one record.
#[test]
fn pattern_paint_is_stored_and_round_trips() {
    bounded_case("pattern_paint_is_stored_and_round_trips", || {
        // Tile 20 x 12 px at (4, 8) in a space moved by (8, 12): the cell origin is (12, 20) px =
        // (9, 15) pt; the tile is 15 x 9 pt. The artwork (circle, square) lies inside the tile.
        let svg = doc(
            r##"<pattern id="dots" patternUnits="userSpaceOnUse" x="4" y="8" width="20" height="12" patternTransform="translate(8 12)">
                  <circle cx="5" cy="5" r="3" fill="#ff0000"/>
                  <rect x="12" y="2" width="6" height="6" fill="#0000ff"/>
                </pattern>"##,
            r##"<rect id="a" x="10" y="10" width="60" height="40" fill="url(#dots) #00ff00" stroke="#000000" stroke-width="2"/>"##,
        );
        let p = import(&svg);
        validate(&p);
        let r = &p.vector_records;
        assert_eq!((r.patterns.len(), r.pattern_paints.len()), (1, 1));
        assert!(r.clips.is_empty() && r.masks.is_empty() && r.attachments.is_empty());
        let StudioPatternSource::Definition {
            content_layer_id,
            tile_width,
            tile_height,
            move_tile_with_art,
            ..
        } = &r.patterns[0].source
        else {
            panic!("definition model");
        };
        assert!(close(tile_width.value, 15.0) && close(tile_height.value, 9.0));
        assert!(*move_tile_with_art);
        let a = layer(&p, "a");
        let style = p.layer_style(&a.layer_id).unwrap();
        let binding = &r.pattern_paints[0];
        assert_eq!(
            (binding.style_id.as_str(), binding.row),
            (style.style_id.as_str(), 0)
        );
        let StudioPatternPlacement::Matrix { matrix } = &binding.placement else {
            panic!("matrix placement");
        };
        for (got, want) in matrix.iter().zip([1.0, 0.0, 0.0, 1.0, 9.0, 15.0]) {
            assert!(close(*got, want), "{matrix:?}");
        }
        // The row keeps the author's fallback colour as stand-in, the stroke row is untouched.
        let [
            StudioAppearanceRow::Fill {
                paint: StudioPaint::Solid { colour },
                ..
            },
            StudioAppearanceRow::Stroke { .. },
        ] = style.rows.as_slice()
        else {
            panic!("{:?}", style.rows);
        };
        assert!(close(colour.g, 1.0) && close(colour.r, 0.0));
        // The tile artwork is a hidden group of two vector layers.
        let tile = layer_by_id(&p, content_layer_id);
        assert!(!tile.visible);
        assert_eq!(geometry_below(&p, content_layer_id).len(), 2);
        assert!(p.has_loss_code("pattern_fallback_color_used"));
        assert!(!p.has_loss_code("pattern_paint_not_representable"));
        assert!(!p.has_loss_code("pattern_definition_not_stored"));

        let opts = ExportOptions::default();
        let out = export_proposal_with_loss(&p, &opts).unwrap();
        for needle in [
            "<pattern id=\"p1\" patternUnits=\"userSpaceOnUse\"",
            "patternTransform=\"matrix(",
            "fill=\"url(#p1) #00ff00\"",
        ] {
            assert!(out.text.contains(needle), "{needle}: {}", out.text);
        }
        // One element in the body, two in the pattern; the hidden tile group is not an element.
        assert_eq!(out.text.matches("<path ").count(), 3, "{}", out.text);
        round_trip(&p);

        // objectBoundingBox tile units (the default): tile = 1/4 x 1/2 of the box of each user.
        // Boxes (10,10,40,40) and (60,10,40,40) share one record (tile 10 x 20 px) with two
        // placements; a box of another size is a record of its own.
        let svg = doc(
            r##"<pattern id="fr" width="0.25" height="0.5"><rect width="10" height="10" fill="#123456"/></pattern>"##,
            r##"<rect id="b1" x="10" y="10" width="40" height="40" fill="url(#fr)"/>
                <rect id="b2" x="60" y="10" width="40" height="40" fill="url(#fr)"/>
                <rect id="b3" x="10" y="60" width="20" height="20" fill="url(#fr)"/>"##,
        );
        let p = import(&svg);
        validate(&p);
        let r = &p.vector_records;
        assert_eq!(
            (r.patterns.len(), r.pattern_paints.len()),
            (2, 3),
            "{:?}",
            p.loss
        );
        let placements: Vec<[f64; 6]> = r
            .pattern_paints
            .iter()
            .map(|b| match &b.placement {
                StudioPatternPlacement::Matrix { matrix } => *matrix,
                StudioPatternPlacement::Parametric { .. } => panic!("parametric"),
            })
            .collect();
        // b1 origin (10, 10) px = (7.5, 7.5) pt; b2 (60, 10) px = (45, 7.5) pt.
        assert!(close(placements[0][4], 7.5) && close(placements[1][4], 45.0));
        // No fallback was given: the stand-in is the first solid colour of the tile.
        assert!(p.has_loss_code("pattern_stand_in_derived"));
        let b1 = p.layer_style(&layer(&p, "b1").layer_id).unwrap();
        assert!(matches!(
            b1.rows.as_slice(),
            [StudioAppearanceRow::Fill { paint: StudioPaint::Solid { colour }, .. }]
                if close(colour.r, f64::from(0x12u8) / 255.0)
        ));
        round_trip(&p);

        // Content that leaves the tile is clipped to the cell (SVG overflow: hidden): a
        // tile-cell clip is stored and attached to the tile group, and the export relies on the
        // pattern default instead of writing it out, so re-import finds exactly one again.
        let overflowing = |attrs: &str| {
            doc(
                &format!(
                    r##"<pattern id="o" patternUnits="userSpaceOnUse" width="20" height="20" {attrs}><rect width="30" height="10" fill="#ff0000"/></pattern>"##
                ),
                r##"<rect id="r1" width="50" height="50" fill="url(#o)"/>"##,
            )
        };
        let p = import(&overflowing(""));
        validate(&p);
        assert_eq!(
            (
                p.vector_records.clips.len(),
                p.vector_records.attachments.len()
            ),
            (1, 1)
        );
        let q = round_trip(&p);
        assert_eq!(q.vector_records.clips.len(), 1);
        assert_eq!(q.vector_records.attachments.len(), 1);
        // `overflow: visible` is read as CSS (V4-08): attribute, escaped property name, case and
        // whitespace all apply; a commented-out declaration does not.
        for (attrs, clipped) in [
            (r##"overflow="visible""##, false),
            (r##"style="o\76 erflow:visible""##, false),
            (r##"style="OVERFLOW : VISIBLE ;""##, false),
            (r##"style="/* overflow:visible */ opacity:1""##, true),
            (r##"overflow="hidden""##, true),
        ] {
            let p = import(&overflowing(attrs));
            assert_eq!(
                p.vector_records.clips.len(),
                usize::from(clipped),
                "{attrs}"
            );
        }
    });
}

/// SVG 1.1 14.3: a clipPath that is not provably a no-op is a hidden group of its member shapes
/// (winding = clip-rule) plus `StudioVectorClip` and an attachment; `clip-path` on the clipPath
/// and on a member nest as attachments of the group and the member; mixed rules and members
/// that are not shapes stay typed losses.
#[test]
fn clip_paths_nested_evenodd_round_trip() {
    bounded_case("clip_paths_nested_evenodd_round_trip", || {
        // `outer`: a pentagram (self-overlapping, evenodd) further clipped by `win` (0..90).
        // `inner` clips the rectangle `big` (0..150) to 10..50.
        let svg = doc_sized(
            200,
            r##"<clipPath id="outer" clip-path="url(#win)">
                  <path id="star" d="M50 10 L80 100 L5 45 L95 45 L20 100 Z" clip-rule="evenodd"/>
                </clipPath>
                <clipPath id="win"><rect x="0" y="0" width="90" height="90"/></clipPath>
                <clipPath id="inner"><rect x="10" y="10" width="40" height="40"/></clipPath>"##,
            r##"<g id="g1" clip-path="url(#outer)"><rect id="big" x="0" y="0" width="150" height="150" fill="#336699" clip-path="url(#inner)"/></g>"##,
        );
        let p = import(&svg);
        validate(&p);
        let r = &p.vector_records;
        assert_eq!(r.clips.len(), 3, "{:?}", p.loss);
        assert_eq!(r.attachments.len(), 3);
        assert_eq!(
            r.clips
                .iter()
                .filter(|c| c.clip_rule == StudioClipRule::EvenOdd)
                .count(),
            1
        );
        // The clipped layers are the document's own; the clip sources are hidden groups.
        for name in ["g1", "big"] {
            let a = r.attachment(&layer(&p, name).layer_id).unwrap();
            assert!(a.clip_id.is_some() && a.mask_id.is_none(), "{name}");
        }
        for c in &r.clips {
            assert!(!layer_by_id(&p, &c.source_layer_id).visible);
        }
        // The pentagram path keeps its winding.
        let star: Vec<String> = r
            .clips
            .iter()
            .filter(|c| c.clip_rule == StudioClipRule::EvenOdd)
            .flat_map(|c| geometry_below(&p, &c.source_layer_id))
            .collect();
        assert!(
            star.len() == 1 && star[0].starts_with("EvenOdd/true/"),
            "{star:?}"
        );
        // What used to be reported is gone.
        for code in ["clip_path_not_representable", "clip_rule_evenodd"] {
            assert!(!p.has_loss_code(code), "{code}: {:?}", p.loss);
        }
        assert!(
            !p.has_loss_code("clip_path_definition_not_stored"),
            "{:?}",
            p.loss
        );

        let out = export_proposal_with_loss(&p, &ExportOptions::default()).unwrap();
        assert!(out.text.contains("clip-rule=\"evenodd\""), "{}", out.text);
        assert_eq!(out.text.matches("<clipPath ").count(), 3, "{}", out.text);
        // Three references: the group, the rectangle and the outer clipPath's own.
        assert_eq!(
            out.text.matches("clip-path=\"url(#").count(),
            3,
            "{}",
            out.text
        );
        // Body: the rectangle; defs: star, window, inner.
        assert_eq!(out.text.matches("<path ").count(), 4, "{}", out.text);
        round_trip(&p);

        // The same clip used by two elements at the same place is one record; at another place
        // (a different matrix) it is another, because the geometry is baked.
        let svg = doc(
            r##"<clipPath id="c"><path d="M0 0 L40 0 L0 40 Z"/></clipPath>"##,
            r##"<rect id="u1" width="60" height="60" clip-path="url(#c)"/>
                <rect id="u2" width="60" height="60" clip-path="url(#c)"/>
                <rect id="u3" width="60" height="60" transform="translate(30 30)" clip-path="url(#c)"/>"##,
        );
        let p = import(&svg);
        validate(&p);
        assert_eq!(
            (
                p.vector_records.clips.len(),
                p.vector_records.attachments.len()
            ),
            (2, 3)
        );
        round_trip(&p);

        // Typed losses stay typed: mixed clip rules, a text member, objectBoundingBox with a
        // transform (the order of the two maps is not settled), more members than the cap.
        let member = |inner: &str| {
            doc(
                &format!(r##"<clipPath id="k">{inner}</clipPath>"##),
                r##"<rect id="r" x="40" y="10" width="30" height="30" clip-path="url(#k)"/>"##,
            )
        };
        let cases = [
            (
                r##"<path clip-rule="evenodd" d="M0 0L50 0L50 50Z"/><path clip-rule="nonzero" d="M60 0L100 0L100 50Z"/>"##.to_owned(),
                "clip_rule_mixed",
            ),
            (r##"<text>x</text>"##.to_owned(), "clip_member_unsupported"),
            (
                (0..70).map(|_| r##"<rect width="1" height="1"/>"##).collect::<String>(),
                "clip_too_many_members",
            ),
        ];
        for (inner, reason) in cases {
            let p = import(&member(&inner));
            assert!(p.vector_records.clips.is_empty(), "{reason}");
            assert!(
                p.has_loss_code("clip_path_not_representable") && p.has_loss_code(reason),
                "{reason}: {:?}",
                p.loss
            );
            assert!(layer(&p, "r").visible);
        }
        // The clip rule is read as CSS (V4-08): an escaped property name, an upper-case keyword and
        // whitespace apply; a commented-out declaration does not.
        let rule_of = |attrs: &str| {
            let p = import(&doc(
                &format!(r##"<clipPath id="c"><path d="M0 0L50 0L50 50Z" {attrs}/></clipPath>"##),
                r##"<rect id="r" x="10" y="0" width="30" height="30" clip-path="url(#c)"/>"##,
            ));
            assert_eq!(p.vector_records.clips.len(), 1, "{attrs}: {:?}", p.loss);
            p.vector_records.clips[0].clip_rule
        };
        for (attrs, want) in [
            (r##"style="clip-r\75 le:evenodd""##, StudioClipRule::EvenOdd),
            (
                r##"style="CLIP-RULE : EvenOdd !important""##,
                StudioClipRule::EvenOdd,
            ),
            (r##"clip-rule="EVENODD""##, StudioClipRule::EvenOdd),
            (
                r##"style="/* clip-rule:evenodd */""##,
                StudioClipRule::NonZero,
            ),
            (
                r##"style="clip-rule:evenodd" clip-rule="nonzero""##,
                StudioClipRule::EvenOdd,
            ),
        ] {
            assert_eq!(rule_of(attrs), want, "{attrs}");
        }
        let units = doc(
            r##"<clipPath id="k" clipPathUnits="objectBoundingBox" transform="translate(0.1 0)"><rect width="0.5" height="1"/></clipPath>"##,
            r##"<rect id="r" x="10" y="10" width="30" height="30" clip-path="url(#k)"/>"##,
        );
        let p = import(&units);
        assert!(p.vector_records.clips.is_empty() && p.has_loss_code("clip_units_not_stored"));
    });
}

/// SVG 1.1 14.4 and CSS Masking 1 9.2: a mask is a hidden group holding its content (paint
/// included) plus `StudioVectorMask` with the mode from `mask-type` and the region in document
/// points; the export writes `mask-type`, a `userSpaceOnUse` region and the content again.
#[test]
fn masks_luminance_and_alpha_round_trip() {
    bounded_case("masks_luminance_and_alpha_round_trip", || {
        // `lum`: default luminance mask, default region (-10%..110% of the box 10..90 = 2..98 px,
        // 1.5..73.5 pt, so 72 pt wide), content a gradient rectangle. `al`: alpha mask with a
        // half-transparent circle, region 100..200 px = 75..150 pt.
        let svg = String::from(
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="200" height="100" viewBox="0 0 200 100"><defs>
              <linearGradient id="fade"><stop offset="0" stop-color="#ffffff"/><stop offset="1" stop-color="#000000"/></linearGradient>
              <mask id="lum"><rect x="0" y="0" width="100" height="100" fill="url(#fade)"/></mask>
              <mask id="al" mask-type="alpha" maskUnits="userSpaceOnUse" x="100" y="0" width="100" height="100"><circle cx="150" cy="50" r="40" fill="#000000" fill-opacity="0.5"/></mask>
            </defs>
            <rect id="left" x="10" y="10" width="80" height="80" fill="#ff8800" mask="url(#lum)"/>
            <rect id="right" x="110" y="10" width="80" height="80" fill="#0088ff" mask="url(#al)"/></svg>"##,
        );
        let p = import(&svg);
        validate(&p);
        let r = &p.vector_records;
        assert_eq!((r.masks.len(), r.attachments.len()), (2, 2), "{:?}", p.loss);
        for code in [
            "mask_not_representable",
            "mask_type_luminance",
            "mask_type_alpha",
            "mask_definition_not_stored",
        ] {
            assert!(!p.has_loss_code(code), "{code}: {:?}", p.loss);
        }
        let mask_of = |name: &str| {
            let a = r.attachment(&layer(&p, name).layer_id).unwrap();
            assert!(a.clip_id.is_none());
            r.mask(a.mask_id.as_deref().unwrap()).unwrap()
        };
        let (lum, al) = (mask_of("left"), mask_of("right"));
        assert_eq!(
            (lum.mode, al.mode),
            (StudioMaskMode::Luminance, StudioMaskMode::Alpha)
        );
        assert!(!lum.invert_mask && !lum.mask_clip);
        let region = |m: &hsk_studio_folio::StudioVectorMask| {
            let b = m.region.as_ref().unwrap();
            [b.x.value, b.y.value, b.width.value, b.height.value]
        };
        for (got, want) in region(lum).iter().zip([1.5, 1.5, 72.0, 72.0]) {
            assert!(close(*got, want), "{:?}", region(lum));
        }
        for (got, want) in region(al).iter().zip([75.0, 0.0, 75.0, 75.0]) {
            assert!(close(*got, want), "{:?}", region(al));
        }
        // The mask content keeps its paint: the luminance source is a gradient fill.
        let content = geometry_below(&p, &lum.source_layer_id);
        assert_eq!(content.len(), 1);
        let source = layer_by_id(&p, &lum.source_layer_id);
        assert!(!source.visible);
        let child = p
            .snapshot
            .document()
            .layers
            .iter()
            .find(|l| matches!(&l.parent, hsk_studio_folio::Parent::Node(n) if n.node_id == source.layer_id))
            .unwrap();
        assert!(matches!(
            p.layer_style(&child.layer_id).unwrap().rows.as_slice(),
            [StudioAppearanceRow::Fill {
                paint: StudioPaint::Gradient { .. },
                ..
            }]
        ));

        let out = export_proposal_with_loss(&p, &ExportOptions::default()).unwrap();
        assert_eq!(out.text.matches("<mask ").count(), 2, "{}", out.text);
        assert_eq!(out.text.matches("mask-type=\"alpha\"").count(), 1);
        assert_eq!(out.text.matches("maskUnits=\"userSpaceOnUse\"").count(), 2);
        assert_eq!(
            out.text.matches(" mask=\"url(#m").count(),
            2,
            "{}",
            out.text
        );
        // Body: two rectangles; defs: the gradient rectangle and the circle.
        assert_eq!(out.text.matches("<path ").count(), 4, "{}", out.text);
        let q = round_trip(&p);
        assert_eq!(q.vector_records.masks.len(), 2);
    });
}

/// Hostile and unsupported structures stay bounded and typed (V4-01/02): reference loops, a
/// definition that exceeds its element allowance, absurd tile sizes, an unusable `mask-type`.
#[test]
fn records_stay_bounded_and_typed() {
    bounded_case("records_stay_bounded_and_typed", || {
        // A mask whose content uses the mask itself: the outer use is stored, the inner one is a
        // reported loop (SVG 2 16.1.2), not a recursion.
        let looped = doc(
            r##"<mask id="m" maskUnits="userSpaceOnUse" x="-50" y="-50" width="300" height="300"><rect x="5" y="5" width="20" height="20" fill="#fff" mask="url(#m)"/><circle cx="50" cy="50" r="9" fill="#fff"/></mask>"##,
            r##"<rect id="e" x="10" y="10" width="40" height="40" mask="url(#m)"/>"##,
        );
        let p = import(&looped);
        validate(&p);
        assert_eq!(p.vector_records.masks.len(), 1, "{:?}", p.loss);
        assert!(p.has_loss_code("mask_reference_cycle"), "{:?}", p.loss);

        // The same for a pattern whose tile paints with itself, and a clip inside its own member.
        let self_pattern = doc(
            r##"<pattern id="s" patternUnits="userSpaceOnUse" width="10" height="10"><rect width="5" height="5" fill="url(#s) #abcdef"/></pattern>"##,
            r##"<rect id="e" width="40" height="40" fill="url(#s)"/>"##,
        );
        let p = import(&self_pattern);
        validate(&p);
        assert_eq!(p.vector_records.patterns.len(), 1);
        assert!(p.has_loss_code("pattern_reference_cycle"), "{:?}", p.loss);
        let self_clip = doc(
            r##"<clipPath id="c"><rect width="30" height="30" clip-path="url(#c)"/></clipPath>"##,
            r##"<rect id="e" x="10" y="10" width="60" height="60" clip-path="url(#c)"/>"##,
        );
        let p = import(&self_clip);
        validate(&p);
        assert!(p.has_loss_code("clip_path_reference_cycle"), "{:?}", p.loss);

        // Nesting stops at depth 8: a chain of 12 patterns, each tile painted with the next one,
        // stores the first 8 and reports the rest.
        let chain: String = (0..12)
            .map(|i| {
                if i == 11 {
                    r##"<pattern id="t11" patternUnits="userSpaceOnUse" width="10" height="10"><rect width="5" height="5" fill="#336699"/></pattern>"##.to_owned()
                } else {
                    format!(
                        r##"<pattern id="t{i}" patternUnits="userSpaceOnUse" width="10" height="10"><rect width="5" height="5" fill="url(#t{})"/></pattern>"##,
                        i + 1
                    )
                }
            })
            .collect();
        let p = import(&doc(
            &chain,
            r##"<rect id="e" width="40" height="40" fill="url(#t0)"/>"##,
        ));
        validate(&p);
        assert_eq!(p.vector_records.patterns.len(), 8, "{:?}", p.loss);
        assert!(p.has_loss_code("pattern_depth_exceeded"), "{:?}", p.loss);

        // Aggregate budget: definition content has its own allowance (`max_nodes` elements in
        // total, here 20; the file has 17 XML nodes). The first use of a 10-rectangle mask costs
        // 11 and fits, later uses at other places do not and stay typed; the import itself
        // succeeds.
        let rects: String = (0..10)
            .map(|i| {
                format!(
                    r##"<rect x="{}" y="0" width="5" height="5" fill="#fff"/>"##,
                    i * 6
                )
            })
            .collect();
        let uses: String = (0..4)
            .map(|i| format!(r##"<rect id="u{i}" width="20" height="20" transform="translate({} 0)" mask="url(#m)"/>"##, i * 25))
            .collect();
        let budget = doc(
            &format!(
                r##"<mask id="m" maskUnits="userSpaceOnUse" x="-1000" y="-1000" width="3000" height="3000">{rects}</mask>"##
            ),
            &uses,
        );
        let mut small = ImportPolicy::default();
        small.limits.max_nodes = 20;
        let p = import_with(&budget, &small).unwrap();
        validate(&p);
        assert_eq!(p.vector_records.masks.len(), 1, "{:?}", p.loss);
        assert_eq!(count(&p, "mask_not_representable"), 3);
        assert!(p.has_loss_code("definition_budget"));
        // With the default budget every distinct use is stored.
        let p = import(&budget);
        assert_eq!(
            (
                p.vector_records.masks.len(),
                p.vector_records.attachments.len()
            ),
            (4, 4)
        );

        // Absurd numbers: a tile of 1e300 and a mask region at the edge of f64 are not stored.
        let huge_tile = doc(
            r##"<pattern id="h" patternUnits="userSpaceOnUse" width="1e300" height="10"><rect width="5" height="5"/></pattern>"##,
            r##"<rect id="e" width="40" height="40" fill="url(#h)"/>"##,
        );
        let p = import(&huge_tile);
        assert!(p.vector_records.patterns.is_empty());
        assert!(
            p.has_loss_code("pattern_geometry_out_of_range")
                && p.has_loss_code("pattern_paint_not_representable")
        );
        let unusable = doc(
            r##"<mask id="m" mask-type="bogus" maskUnits="userSpaceOnUse" x="0" y="0" width="100" height="100"><rect width="40" height="40" fill="#888"/></mask>"##,
            r##"<rect id="e" width="60" height="60" mask="url(#m)"/>"##,
        );
        let p = import(&unusable);
        assert!(p.vector_records.masks.is_empty());
        assert!(p.has_loss_code("mask_type_unusable") && p.has_loss_code("mask_not_representable"));

        // Export refuses records that do not match the document instead of writing them.
        let mut broken = import(&doc(
            r##"<clipPath id="c"><path d="M0 0 L40 0 L0 40 Z"/></clipPath>"##,
            r##"<rect id="u1" width="60" height="60" clip-path="url(#c)"/>"##,
        ));
        broken.vector_records.attachments[0].clip_id = Some("nope".into());
        assert!(matches!(
            export_proposal(&broken, &ExportOptions::default()),
            Err(VectorError::Folio(_))
        ));
    });
}
