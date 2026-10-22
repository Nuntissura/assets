//! Slice 2 contract: gradients, appearance rows, stroke properties and SVGZ with a bomb cap.
//! Synthetic SVGs only; every expected number is computed by hand in the comments.
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{
    BLEND_NORMAL, Budget, GeometryUnit, GradientInterpolation, GradientSpread, Length,
    ProfileBinding, StrokeAlign, StrokeCap, StrokeJoin, StudioAppearanceRow, StudioColor,
    StudioGradientGeometry, StudioGradientStop, StudioPaint, StudioPaintStyle, Unresolved,
    validate_design_records,
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

fn import_bytes(bytes: &[u8], policy: &ImportPolicy) -> Result<VectorProposal> {
    import_svg(bytes, policy, &mut SequentialIdMint::new(3), &CancellationToken::default())
}

fn import(svg: &str) -> VectorProposal {
    import_bytes(svg.as_bytes(), &ImportPolicy::default()).unwrap()
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

fn assert_matrix(got: [f64; 6], want: [f64; 6]) {
    assert!(got.iter().zip(want).all(|(g, w)| close(*g, w)), "{got:?} != {want:?}");
}

fn colour(r: u8, g: u8, b: u8) -> StudioColor {
    StudioColor {
        r: f64::from(r) / 255.0,
        g: f64::from(g) / 255.0,
        b: f64::from(b) / 255.0,
        colour_profile_id: ProfileBinding::Unassigned(()),
    }
}

fn solid(r: u8, g: u8, b: u8) -> StudioPaint {
    StudioPaint::Solid {
        colour: colour(r, g, b),
    }
}

fn pt(value: f64) -> Length {
    Length {
        value,
        unit: GeometryUnit::Pt,
    }
}

/// A gradient stop as Folio stores it: position and opacity are percent, no midpoint.
fn stop(position: f64, opacity: f64, (r, g, b): (u8, u8, u8)) -> StudioGradientStop {
    StudioGradientStop {
        position,
        midpoint: None,
        opacity,
        colour: colour(r, g, b),
    }
}

fn fill_row(paint: StudioPaint, opacity: f64) -> StudioAppearanceRow {
    StudioAppearanceRow::Fill {
        paint,
        opacity,
        blend_mode: BLEND_NORMAL,
        visible: true,
    }
}

/// The style of the (single-path) layer named `name`, read through its Folio design entry.
fn style_of<'a>(p: &'a VectorProposal, name: &str) -> Option<&'a StudioPaintStyle> {
    let layer = p.snapshot.document().layers.iter().find(|l| l.name == name)?;
    p.layer_style(&layer.layer_id)
}

/// The paint and row opacity (percent) of the first fill row.
fn first_fill(style: &StudioPaintStyle) -> Option<(&StudioPaint, f64)> {
    style.rows.iter().find_map(|r| match r {
        StudioAppearanceRow::Fill { paint, opacity, .. } => Some((paint, *opacity)),
        StudioAppearanceRow::Stroke { .. } => None,
    })
}

fn assert_paint_close(a: &StudioPaint, b: &StudioPaint) {
    let stops_close = |x: &[StudioGradientStop], y: &[StudioGradientStop]| {
        x.len() == y.len()
            && x.iter().zip(y).all(|(s, t)| {
                close(s.position, t.position)
                    && close(s.opacity, t.opacity)
                    && s.midpoint == t.midpoint
                    && s.colour == t.colour
            })
    };
    match (a, b) {
        (StudioPaint::Solid { colour: x }, StudioPaint::Solid { colour: y }) => assert_eq!(x, y),
        (StudioPaint::Gradient { gradient: x }, StudioPaint::Gradient { gradient: y }) => {
            match (&x.geometry, &y.geometry) {
                (
                    StudioGradientGeometry::Linear { x1, y1, x2, y2 },
                    StudioGradientGeometry::Linear { x1: a1, y1: b1, x2: a2, y2: b2 },
                ) => assert_matrix([*x1, *y1, *x2, *y2, 0.0, 0.0], [*a1, *b1, *a2, *b2, 0.0, 0.0]),
                (
                    StudioGradientGeometry::Radial { cx, cy, r, fx, fy, fr },
                    StudioGradientGeometry::Radial { cx: a, cy: b, r: c, fx: d, fy: e, fr: f },
                ) => assert_matrix([*cx, *cy, *r, *fx, *fy, *fr], [*a, *b, *c, *d, *e, *f]),
                _ => panic!("gradient kinds differ: {:?} vs {:?}", x.geometry, y.geometry),
            }
            assert_matrix(x.transform, y.transform);
            assert!(x.spread == y.spread && x.interpolation == y.interpolation);
            assert!(stops_close(&x.stops, &y.stops));
        }
        _ => panic!("paint kinds differ: {a:?} vs {b:?}"),
    }
}

const GRADIENTS: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="200" height="100" viewBox="0 0 200 100">
  <defs>
    <linearGradient id="a" x1="0" y1="0" x2="1" y2="0" gradientTransform="translate(10 0)" spreadMethod="reflect">
      <stop offset="0" stop-color="#ff0000"/>
      <stop offset="50%" stop-color="#00ff00" stop-opacity="0.5"/>
      <stop offset="1" style="stop-color:#0000ff"/>
    </linearGradient>
    <radialGradient id="b" href="#a" gradientUnits="userSpaceOnUse" cx="100" cy="50" r="40"/>
  </defs>
  <rect id="box" x="20" y="10" width="40" height="20" fill="url(#a)"/>
  <circle id="disc" cx="100" cy="50" r="40" fill="url(#b)" fill-opacity="0.5"/>
</svg>"##;

#[test]
fn gradients_units_transform_spread_stops() {
    bounded_case("gradients_units_transform_spread_stops", || {
        let p = import(GRADIENTS);
        assert!(
            p.loss.iter().all(|l| !l.code.starts_with("unsupported")),
            "{:?}",
            p.loss
        );
        let red = (255, 0, 0);

        // Rect: objectBoundingBox on bbox (20,10)-(60,30): unit square -> (20 + 40u, 10 + 20v);
        // gradientTransform translate(10,0) acts first: x -> x + 10. px -> pt factor 0.75:
        // a = 0.75*40 = 30, d = 0.75*20 = 15, e = 0.75*(20 + 40*10) = 315, f = 0.75*10 = 7.5.
        let style = style_of(&p, "box").unwrap();
        let Some((StudioPaint::Gradient { gradient: g }, row_opacity)) = first_fill(style) else {
            panic!("box fill is not a gradient: {:?}", style.rows);
        };
        assert_eq!(row_opacity, 100.0, "element opacity is folded into the stops");
        assert_eq!(p.gradient_units(&style.style_id, 0), Some(GradientUnits::ObjectBoundingBox));
        assert_eq!(
            g.geometry,
            StudioGradientGeometry::Linear { x1: 0.0, y1: 0.0, x2: 1.0, y2: 0.0 }
        );
        assert_matrix(g.transform, [30.0, 0.0, 0.0, 15.0, 315.0, 7.5]);
        assert_eq!(g.spread, GradientSpread::Reflect);
        assert_eq!((g.interpolation, g.dither), (GradientInterpolation::Classic, false));
        assert_eq!(
            g.stops,
            vec![
                stop(0.0, 100.0, red),
                stop(50.0, 50.0, (0, 255, 0)), // offset 50%, stop-opacity 0.5
                stop(100.0, 100.0, (0, 0, 255)), // colour from the style attribute
            ]
        );

        // Circle: radial "b" inherits stops, spread and gradientTransform from "a" through href;
        // userSpaceOnUse: transform = 0.75 * translate(10,0) = [0.75 0 0 0.75 7.5 0];
        // fill-opacity 0.5 halves every stop opacity.
        let style = style_of(&p, "disc").unwrap();
        let Some((StudioPaint::Gradient { gradient: r }, row_opacity)) = first_fill(style) else {
            panic!("disc fill is not a gradient: {:?}", style.rows);
        };
        assert_eq!(row_opacity, 100.0);
        assert_eq!(p.gradient_units(&style.style_id, 0), Some(GradientUnits::UserSpaceOnUse));
        assert_eq!(
            r.geometry,
            StudioGradientGeometry::Radial {
                cx: 100.0,
                cy: 50.0,
                r: 40.0,
                fx: 100.0,
                fy: 50.0,
                fr: 0.0
            }
        );
        assert_matrix(r.transform, [0.75, 0.0, 0.0, 0.75, 7.5, 0.0]);
        assert_eq!(r.spread, GradientSpread::Reflect);
        assert_eq!(
            r.stops,
            vec![
                stop(0.0, 50.0, red),
                stop(50.0, 25.0, (0, 255, 0)),
                stop(100.0, 50.0, (0, 0, 255)),
            ]
        );

        // Export writes userSpaceOnUse gradients with the full matrix; reopening keeps the paint.
        let text = export_proposal(&p, &ExportOptions::default()).unwrap();
        for needle in ["<linearGradient", "<radialGradient", "spreadMethod=\"reflect\"", "url(#g1)"] {
            assert!(text.contains(needle), "missing {needle} in {text}");
        }
        let again = import(&text);
        for name in ["box", "disc"] {
            let (x, y) = (style_of(&p, name).unwrap(), style_of(&again, name).unwrap());
            let (xp, xo) = first_fill(x).unwrap();
            let (yp, yo) = first_fill(y).unwrap();
            assert_paint_close(xp, yp);
            assert!(close(xo, yo));
        }
    });
}

const STROKES: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="200" height="100">
  <path id="dash" d="M0 0 L100 0" fill="none" stroke="#000000" stroke-width="8" stroke-linecap="square" stroke-linejoin="bevel" stroke-miterlimit="10" stroke-dasharray="8 4 2" stroke-dashoffset="3"/>
  <rect id="under" x="0" y="20" width="10" height="10" fill="#ff0000" stroke="#0000ff" stroke-width="2" paint-order="stroke"/>
  <path id="odd" d="M0 50 L10 50" fill="none" stroke="#000" stroke-miterlimit="0.5" stroke-linejoin="miter-clip"/>
  <path id="big" d="M0 60 L10 60" fill="none" stroke="#000" stroke-miterlimit="600"/>
</svg>"##;

#[test]
fn appearance_rows_and_stroke_properties() {
    bounded_case("appearance_rows_and_stroke_properties", || {
        let p = import(STROKES);
        // 0.75 pt per px: width 8 -> 6, dash "8 4 2" is odd so it repeats to 8 4 2 8 4 2 ->
        // 6 3 1.5 6 3 1.5, offset 3 -> 2.25; `square` is the spec's projecting cap.
        let dash = style_of(&p, "dash").unwrap();
        let [StudioAppearanceRow::Stroke { paint, stroke, opacity, blend_mode, visible }] =
            dash.rows.as_slice()
        else {
            panic!("expected one stroke row: {:?}", dash.rows);
        };
        assert_eq!(paint, &solid(0, 0, 0));
        assert_eq!((*opacity, *blend_mode, *visible), (100.0, BLEND_NORMAL, true));
        assert_eq!(stroke.weight, pt(6.0));
        assert_eq!((stroke.cap, stroke.join), (StrokeCap::Projecting, StrokeJoin::Bevel));
        assert_eq!(stroke.align, StrokeAlign::Center);
        assert_eq!(stroke.cap.wire(), 3);
        assert_eq!(stroke.miter_limit, 10.0);
        assert_eq!(stroke.dash, [6.0, 3.0, 1.5, 6.0, 3.0, 1.5].map(pt));
        assert_eq!(stroke.dash_offset, pt(2.25));

        // paint-order: stroke puts the stroke row below the fill row.
        let under = style_of(&p, "under").unwrap();
        assert!(matches!(
            under.rows.as_slice(),
            [StudioAppearanceRow::Stroke { .. }, StudioAppearanceRow::Fill { .. }]
        ));

        // Invalid or out-of-range values are typed, not dropped silently.
        assert!(p.has_loss_code("stroke_miterlimit_unparsed"));
        assert!(p.has_loss_code("stroke_linejoin_fallback_miter"));
        assert!(p.has_loss_code("stroke_miterlimit_clamped"));
        let Some(StudioAppearanceRow::Stroke { stroke: big, .. }) =
            style_of(&p, "big").unwrap().rows.first()
        else {
            panic!("big has no stroke row");
        };
        assert_eq!(big.miter_limit, 500.0);

        // Export keeps every stroke attribute and the row order; reopening restores them.
        let opts = ExportOptions::default();
        let out = export_proposal_with_loss(&p, &opts).unwrap();
        assert!(out.loss.is_empty(), "{:?}", out.loss);
        assert!(out.text.contains("paint-order=\"stroke\""));
        let again = import(&out.text);
        let Some(StudioAppearanceRow::Stroke { stroke: s2, .. }) =
            style_of(&again, "dash").unwrap().rows.first()
        else {
            panic!("dash lost its stroke");
        };
        assert!(close(s2.weight.value, 6.0) && close(s2.dash_offset.value, 2.25));
        assert_eq!((s2.cap, s2.join, s2.miter_limit), (StrokeCap::Projecting, StrokeJoin::Bevel, 10.0));
        assert!(s2.dash.iter().zip([6.0, 3.0, 1.5, 6.0, 3.0, 1.5]).all(|(a, b)| close(a.value, b)));
        assert!(matches!(
            style_of(&again, "under").unwrap().rows.as_slice(),
            [StudioAppearanceRow::Stroke { .. }, StudioAppearanceRow::Fill { .. }]
        ));

        // A caller-built stack of three rows is not one SVG element: it is written as three
        // path elements under one group and reported as an approximation.
        let mut stacked = p.clone();
        let style = stacked
            .design
            .paint_styles
            .iter_mut()
            .find(|s| s.style_id == under.style_id)
            .unwrap();
        style.rows = vec![
            fill_row(solid(255, 0, 0), 100.0),
            style.rows[0].clone(),
            fill_row(solid(0, 255, 0), 100.0),
        ];
        let out = export_proposal_with_loss(&stacked, &opts).unwrap();
        assert!(out.loss.iter().any(|l| l.code == "appearance_stack_duplicated" && l.kind == LossKind::Approximated));
        assert!(out.text.contains("<g id=\"under\">"), "{}", out.text);
        assert_eq!(out.text.matches("<path").count(), 6, "3 dup rows + dash + odd + big");
        let again = import(&out.text);
        assert_eq!(again.paths.len(), 6);
        assert!(again.snapshot.document().layers.iter().any(|l| l.name == "under" && l.kind == "group"));
    });
}

#[test]
fn svgz_roundtrip_and_decompression_bomb() {
    bounded_case("svgz_roundtrip_and_decompression_bomb", || {
        let source = import(STROKES);
        let text = export_proposal(&source, &ExportOptions::default()).unwrap();
        let gz = gzip_svg(&text);
        assert_eq!(&gz[..3], &[0x1f, 0x8b, 8]);
        assert_eq!(gz, gzip_svg(&text), "gzip output is deterministic");
        let z = import_bytes(&gz, &ImportPolicy::default()).unwrap();
        let plain = import(&text);
        assert_eq!(z.provenance.source_format, "svgz");
        assert_eq!((z.provenance.source_len, z.provenance.decoded_len), (gz.len(), text.len()));
        assert_eq!(z.paths.len(), plain.paths.len());
        assert_eq!(z.paths[0].anchors, plain.paths[0].anchors);

        // Bomb: 3 MiB of spaces deflates to a few KiB (deflate cannot beat ~1000:1) but may not
        // expand past the cap. The same payload is a valid empty SVG under the default cap.
        let bomb_text = format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\"><!--{}--></svg>",
            " ".repeat(3 << 20)
        );
        let bomb = gzip_svg(&bomb_text);
        assert!(bomb.len() < 8 * 1024, "bomb is {} bytes", bomb.len());
        let tight = ImportPolicy {
            limits: Limits {
                max_decompressed_bytes: 64 * 1024,
                ..Limits::default()
            },
            ..ImportPolicy::default()
        };
        assert_eq!(
            import_bytes(&bomb, &tight).unwrap_err(),
            VectorError::BudgetExceeded("decompressed_bytes")
        );
        assert!(import_bytes(&bomb, &ImportPolicy::default()).unwrap().paths.is_empty());
        let small_input = ImportPolicy {
            limits: Limits {
                max_bytes: 100,
                ..Limits::default()
            },
            ..ImportPolicy::default()
        };
        assert_eq!(
            import_bytes(&gz, &small_input).unwrap_err(),
            VectorError::BudgetExceeded("bytes")
        );

        // A damaged trailer or a cut stream is refused with a typed container error.
        let mut bad_crc = gz.clone();
        let n = bad_crc.len();
        bad_crc[n - 8] ^= 0xff;
        assert_eq!(
            import_bytes(&bad_crc, &ImportPolicy::default()).unwrap_err(),
            VectorError::Gzip("checksum")
        );
        let cut = &gz[..gz.len() / 2];
        assert!(matches!(
            import_bytes(cut, &ImportPolicy::default()),
            Err(VectorError::Gzip(_))
        ));
        let mut reserved = gz.clone();
        reserved[3] = 0x20;
        assert_eq!(
            import_bytes(&reserved, &ImportPolicy::default()).unwrap_err(),
            VectorError::Gzip("flags")
        );
    });
}

#[test]
fn gradient_references_hostile_and_degenerate() {
    bounded_case("gradient_references_hostile_and_degenerate", || {
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="100" height="100">
  <defs>
    <linearGradient id="c1" href="#c2"/>
    <linearGradient id="c2" href="#c1"><stop offset="0" stop-color="#112233"/><stop offset="1" stop-color="#445566"/></linearGradient>
    <linearGradient id="ext" xlink:href="http://example.invalid/g.svg#x"/>
    <linearGradient id="one"><stop offset="0.3" stop-color="#00ff00"/></linearGradient>
    <linearGradient id="empty"/>
    <linearGradient id="bb" x1="0" y1="0" x2="1" y2="0"><stop offset="0" stop-color="#000"/><stop offset="1" stop-color="#fff"/></linearGradient>
  </defs>
  <rect id="cycle" width="10" height="10" fill="url(#c1)"/>
  <rect id="external" width="10" height="10" fill="url(#ext)"/>
  <rect id="single" width="10" height="10" fill="url(#one)"/>
  <rect id="nothing" width="10" height="10" fill="url(#empty)"/>
  <rect id="missing" width="10" height="10" fill="url(#nope) #00ff00"/>
  <line id="flat" x1="0" y1="5" x2="50" y2="5" stroke="url(#bb)" stroke-width="2"/>
</svg>"##;
        let p = import(svg);
        assert_eq!(p.paths.len(), 6);

        // A reference cycle ends at the first repeat; the stops of the chain are still used.
        let Some((StudioPaint::Gradient { gradient: g }, _)) =
            first_fill(style_of(&p, "cycle").unwrap())
        else {
            panic!("cycle gradient not resolved");
        };
        assert_eq!(g.stops.len(), 2);
        assert!(p.has_loss_code("gradient_href_cycle"));

        // An href outside the file is denied and never followed; the gradient has no stops left.
        assert!(p.loss.iter().any(|l| l.kind == LossKind::ExternalDenied
            && l.code == "external_href_denied"
            && l.path == "/svg[1]/rect[2]"));
        assert!(style_of(&p, "external").is_none());
        assert!(style_of(&p, "nothing").is_none());
        assert!(p.has_loss_code("gradient_no_stops"));

        // One stop is that colour; a missing id uses the declared fallback and is reported.
        assert_eq!(
            first_fill(style_of(&p, "single").unwrap()),
            Some((&solid(0, 255, 0), 100.0))
        );
        assert_eq!(
            first_fill(style_of(&p, "missing").unwrap()),
            Some((&solid(0, 255, 0), 100.0))
        );
        assert!(p.has_loss_code("unsupported_paint_server"));

        // objectBoundingBox on a zero-height bounding box paints nothing, as SVG defines.
        assert!(style_of(&p, "flat").is_none());
        assert!(p.has_loss_code("gradient_degenerate_bbox"));
    });
}

#[test]
fn design_records_are_the_single_source() {
    bounded_case("design_records_are_the_single_source", || {
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100">
  <defs><pattern id="pt" width="2" height="2"><rect width="1" height="1"/></pattern></defs>
  <rect id="a" x="0" y="0" width="10" height="10" fill="#336699" fill-opacity="0.3" stroke="#000" stroke-width="2" stroke-opacity="0.5"/>
  <rect id="b" x="20" y="0" width="10" height="10" fill="#336699" fill-opacity="0.3" stroke="#000" stroke-width="2" stroke-opacity="0.5"/>
  <rect id="pat" x="40" y="0" width="10" height="10" fill="url(#pt)"/>
</svg>"##;
        let p = import(svg);
        let doc = p.snapshot.document();

        // Folio accepts the record set against the document, with nothing left unresolved.
        let issues = validate_design_records(
            &p.design,
            doc,
            Budget::default(),
            &CancellationToken::default(),
            &Unresolved,
        )
        .unwrap();
        assert!(issues.is_empty(), "{issues:?}");
        assert_eq!(p.design.document_id, doc.document_id);

        // Two identical stacks share one PAINT style; each vector layer names it through its design
        // entry. The pattern fill is stored in the vector records: its layer has a style of its
        // own (the stand-in paint, bound to the pattern) and so has the tile artwork's layer.
        assert_eq!(p.design.paint_styles.len(), 3);
        let style = style_of(&p, "a").unwrap();
        assert_eq!(style_of(&p, "b").unwrap().style_id, style.style_id);
        let stand_in = style_of(&p, "pat").unwrap();
        assert_ne!(stand_in.style_id, style.style_id);
        assert!(p.vector_records.pattern_paint(&stand_in.style_id, 0).is_some());
        assert!(p.has_loss_code("pattern_stand_in_derived") && !p.has_loss_code("pattern_paint_not_representable"));
        assert_eq!(p.vector_records.patterns.len(), 1);
        assert_eq!(p.design.nodes.len(), 4);
        assert_eq!(style.rows.len(), 2);

        // Alpha is row opacity (percent, free of binary noise): fill 0.3 -> 30, stroke 0.5 -> 50.
        let [
            StudioAppearanceRow::Fill { paint: fill, opacity: fo, .. },
            StudioAppearanceRow::Stroke { paint: stroke, opacity: so, .. },
        ] = style.rows.as_slice()
        else {
            panic!("expected fill then stroke: {:?}", style.rows);
        };
        assert_eq!((fill, *fo), (&solid(0x33, 0x66, 0x99), 30.0));
        assert_eq!((stroke, *so), (&solid(0, 0, 0), 50.0));

        // Export reads the records back, and the opacities are a fixed point of the round trip.
        let opts = ExportOptions::default();
        let text = export_proposal(&p, &opts).unwrap();
        for needle in ["fill=\"#336699\"", "fill-opacity=\"0.3\"", "stroke-opacity=\"0.5\""] {
            assert!(text.contains(needle), "missing {needle} in {text}");
        }
        let again = export_proposal(&import(&text), &opts).unwrap();
        assert!(again.contains("fill-opacity=\"0.3\"") && again.contains("stroke-opacity=\"0.5\""));

        // The records are the single source: editing them changes the export, no second copy exists.
        let mut edited = p.clone();
        if let StudioAppearanceRow::Fill { opacity, .. } = &mut edited.design.paint_styles[0].rows[0] {
            *opacity = 60.0;
        }
        assert!(export_proposal(&edited, &opts).unwrap().contains("fill-opacity=\"0.6\""));

        // A hidden row is not drawn and a non-normal blend mode is not expressible per row: both
        // are reported by the export instead of vanishing.
        if let StudioAppearanceRow::Fill { visible, .. } = &mut edited.design.paint_styles[0].rows[0] {
            *visible = false;
        }
        if let StudioAppearanceRow::Stroke { blend_mode, .. } = &mut edited.design.paint_styles[0].rows[1] {
            *blend_mode = 3;
        }
        let out = export_proposal_with_loss(&edited, &opts).unwrap();
        assert!(out.text.contains("fill=\"none\""));
        for code in ["appearance_row_hidden_dropped", "appearance_row_blend_mode_dropped"] {
            assert!(out.loss.iter().any(|l| l.code == code), "missing {code}: {:?}", out.loss);
        }

        // A record Folio refuses (a one-stop gradient) is refused by the export, not written.
        let mut bad = p.clone();
        if let StudioAppearanceRow::Fill { paint, .. } = &mut bad.design.paint_styles[0].rows[0] {
            *paint = StudioPaint::Gradient {
                gradient: hsk_studio_folio::StudioGradient {
                    geometry: StudioGradientGeometry::Linear { x1: 0.0, y1: 0.0, x2: 1.0, y2: 0.0 },
                    transform: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
                    spread: GradientSpread::Pad,
                    interpolation: GradientInterpolation::Classic,
                    dither: false,
                    stops: vec![stop(0.0, 100.0, (0, 0, 0))],
                },
            };
        }
        assert_eq!(
            export_proposal(&bad, &opts).unwrap_err(),
            VectorError::Folio(hsk_studio_folio::Code::InvalidField)
        );

        // A style reference the set does not hold is reported and falls back to the SVG default.
        // The vector records bind a pattern to one of those styles, so they would dangle too and
        // the export refuses them (Folio `DanglingReference`); with the records emptied (the
        // pattern layer stays as a hidden group) the fallback applies.
        let mut dangling = p.clone();
        dangling.design.paint_styles.clear();
        assert_eq!(
            export_proposal_with_loss(&dangling, &opts).unwrap_err(),
            VectorError::Folio(hsk_studio_folio::Code::DanglingReference)
        );
        dangling.vector_records = hsk_studio_folio::StudioVectorRecords::new(dangling.design.document_id.clone());
        let out = export_proposal_with_loss(&dangling, &opts).unwrap();
        assert!(out.loss.iter().any(|l| l.code == "style_ref_unresolved"));
    });
}

#[test]
fn dash_and_design_budgets_are_bounded() {
    bounded_case("dash_and_design_budgets_are_bounded", || {
        // 128 entries are an even list within Folio's 256; 129 repeat to 258 and are refused (solid).
        let dashes = |n: usize| {
            let list = vec!["1"; n].join(" ");
            format!(
                "<svg xmlns=\"http://www.w3.org/2000/svg\"><path d=\"M0 0 L9 9\" fill=\"none\" stroke=\"#000\" stroke-dasharray=\"{list}\"/></svg>"
            )
        };
        let ok = import(&dashes(128));
        let Some(StudioAppearanceRow::Stroke { stroke, .. }) = ok.design.paint_styles[0].rows.first() else {
            panic!("no stroke row");
        };
        assert_eq!(stroke.dash.len(), 128);
        let long = import(&dashes(129));
        assert!(long.has_loss_code("stroke_dasharray_too_long"));
        let Some(StudioAppearanceRow::Stroke { stroke, .. }) = long.design.paint_styles[0].rows.first() else {
            panic!("no stroke row");
        };
        assert!(stroke.dash.is_empty());

        // Many distinct 40-stop gradients (objectBoundingBox maps each rect differently) keep more
        // records than the importer allows: 16 records per allowed XML node. Refused before Folio.
        let stops: String = (0..40)
            .map(|i| format!("<stop offset=\"{}\" stop-color=\"#000\"/>", f64::from(i) / 39.0))
            .collect();
        let rects: String = (0..60)
            .map(|i| format!("<rect x=\"{}\" y=\"0\" width=\"5\" height=\"5\" fill=\"url(#g)\"/>", i * 7))
            .collect();
        let doc = format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\"><defs><linearGradient id=\"g\">{stops}</linearGradient></defs>{rects}</svg>"
        );
        let policy = ImportPolicy {
            limits: Limits {
                max_nodes: 110,
                ..Limits::default()
            },
            ..ImportPolicy::default()
        };
        assert_eq!(
            import_bytes(doc.as_bytes(), &policy).unwrap_err(),
            VectorError::BudgetExceeded("design_records")
        );
        assert_eq!(import(&doc).paths.len(), 60);
    });
}
