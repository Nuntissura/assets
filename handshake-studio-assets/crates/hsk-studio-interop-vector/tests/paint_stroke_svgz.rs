//! Slice 2 contract: gradients, appearance rows, stroke properties and SVGZ with a bomb cap.
//! Synthetic SVGs only; every expected number is computed by hand in the comments.
use hsk_studio_accord::CancellationToken;
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

fn rgba(r: u8, g: u8, b: u8, a: f64) -> Rgba {
    Rgba { r, g, b, a }
}

fn stop(offset: f64, color: Rgba) -> GradientStop {
    GradientStop { offset, color }
}

/// The style of the (single-path) layer named `name`.
fn style_of<'a>(p: &'a VectorProposal, name: &str) -> Option<&'a StyleProposal> {
    let layer = p.snapshot.document().layers.iter().find(|l| l.name == name)?;
    let path = p.paths.iter().find(|x| x.layer_id == layer.layer_id)?;
    let id = path.fill_style_id.as_deref()?;
    p.styles.iter().find(|s| s.style_id == id)
}

fn assert_paint_close(a: &Paint, b: &Paint) {
    let stops_close = |x: &[GradientStop], y: &[GradientStop]| {
        x.len() == y.len()
            && x.iter().zip(y).all(|(s, t)| close(s.offset, t.offset) && s.color == t.color)
    };
    match (a, b) {
        (Paint::Solid(x), Paint::Solid(y)) => assert_eq!(x, y),
        (Paint::Linear(x), Paint::Linear(y)) => {
            assert_matrix(
                [x.x1, x.y1, x.x2, x.y2, 0.0, 0.0],
                [y.x1, y.y1, y.x2, y.y2, 0.0, 0.0],
            );
            assert_matrix(x.transform, y.transform);
            assert!(x.spread == y.spread && stops_close(&x.stops, &y.stops));
        }
        (Paint::Radial(x), Paint::Radial(y)) => {
            assert_matrix([x.cx, x.cy, x.r, x.fx, x.fy, x.fr], [y.cx, y.cy, y.r, y.fx, y.fy, y.fr]);
            assert_matrix(x.transform, y.transform);
            assert!(x.spread == y.spread && stops_close(&x.stops, &y.stops));
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
        let red = rgba(255, 0, 0, 1.0);

        // Rect: objectBoundingBox on bbox (20,10)-(60,30): unit square -> (20 + 40u, 10 + 20v);
        // gradientTransform translate(10,0) acts first: x -> x + 10. px -> pt factor 0.75:
        // a = 0.75*40 = 30, d = 0.75*20 = 15, e = 0.75*(20 + 40*10) = 315, f = 0.75*10 = 7.5.
        let style = style_of(&p, "box").unwrap();
        let Some(Paint::Linear(g)) = style.fills().next() else {
            panic!("box fill is not linear: {:?}", style.items);
        };
        assert_eq!(g.units, GradientUnits::ObjectBoundingBox);
        assert_eq!((g.x1, g.y1, g.x2, g.y2), (0.0, 0.0, 1.0, 0.0));
        assert_matrix(g.transform, [30.0, 0.0, 0.0, 15.0, 315.0, 7.5]);
        assert_eq!(g.spread, SpreadMethod::Reflect);
        assert_eq!(
            g.stops,
            vec![
                stop(0.0, red),
                stop(0.5, rgba(0, 255, 0, 0.5)), // stop-opacity 0.5
                stop(1.0, rgba(0, 0, 255, 1.0)), // colour from the style attribute
            ]
        );

        // Circle: radial "b" inherits stops, spread and gradientTransform from "a" through href;
        // userSpaceOnUse: transform = 0.75 * translate(10,0) = [0.75 0 0 0.75 7.5 0];
        // fill-opacity 0.5 halves every stop alpha.
        let style = style_of(&p, "disc").unwrap();
        let Some(Paint::Radial(r)) = style.fills().next() else {
            panic!("disc fill is not radial: {:?}", style.items);
        };
        assert_eq!(r.units, GradientUnits::UserSpaceOnUse);
        assert_eq!((r.cx, r.cy, r.r, r.fx, r.fy, r.fr), (100.0, 50.0, 40.0, 100.0, 50.0, 0.0));
        assert_matrix(r.transform, [0.75, 0.0, 0.0, 0.75, 7.5, 0.0]);
        assert_eq!(r.spread, SpreadMethod::Reflect);
        assert_eq!(
            r.stops,
            vec![
                stop(0.0, rgba(255, 0, 0, 0.5)),
                stop(0.5, rgba(0, 255, 0, 0.25)),
                stop(1.0, rgba(0, 0, 255, 0.5)),
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
            assert_paint_close(x.fills().next().unwrap(), y.fills().next().unwrap());
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
        let [AppearanceItem::Stroke(s)] = dash.items.as_slice() else {
            panic!("expected one stroke row: {:?}", dash.items);
        };
        assert_eq!(s.style.width_pt, 6.0);
        assert_eq!((s.style.cap, s.style.join), (StrokeCap::Projecting, StrokeJoin::Bevel));
        assert_eq!(s.style.cap.wire(), 3);
        assert_eq!(s.style.miter_limit, 10.0);
        assert_eq!(s.style.dash_pt, vec![6.0, 3.0, 1.5, 6.0, 3.0, 1.5]);
        assert_eq!(s.style.dash_offset_pt, 2.25);

        // paint-order: stroke puts the stroke row below the fill row.
        let under = style_of(&p, "under").unwrap();
        assert!(matches!(under.items.as_slice(), [AppearanceItem::Stroke(_), AppearanceItem::Fill(_)]));

        // Invalid or out-of-range values are typed, not dropped silently.
        assert!(p.has_loss_code("stroke_miterlimit_unparsed"));
        assert!(p.has_loss_code("stroke_linejoin_fallback_miter"));
        assert!(p.has_loss_code("stroke_miterlimit_clamped"));
        let Some(AppearanceItem::Stroke(big)) = style_of(&p, "big").unwrap().items.first() else {
            panic!("big has no stroke row");
        };
        assert_eq!(big.style.miter_limit, 500.0);

        // Export keeps every stroke attribute and the row order; reopening restores them.
        let opts = ExportOptions::default();
        let out = export_proposal_with_loss(&p, &opts).unwrap();
        assert!(out.loss.is_empty(), "{:?}", out.loss);
        assert!(out.text.contains("paint-order=\"stroke\""));
        let again = import(&out.text);
        let Some(AppearanceItem::Stroke(s2)) = style_of(&again, "dash").unwrap().items.first() else {
            panic!("dash lost its stroke");
        };
        assert!(close(s2.style.width_pt, 6.0) && close(s2.style.dash_offset_pt, 2.25));
        assert_eq!((s2.style.cap, s2.style.join, s2.style.miter_limit), (StrokeCap::Projecting, StrokeJoin::Bevel, 10.0));
        assert!(s2.style.dash_pt.iter().zip([6.0, 3.0, 1.5, 6.0, 3.0, 1.5]).all(|(a, b)| close(*a, b)));
        assert!(matches!(
            style_of(&again, "under").unwrap().items.as_slice(),
            [AppearanceItem::Stroke(_), AppearanceItem::Fill(_)]
        ));

        // A caller-built stack of three rows is not one SVG element: it is written as three
        // path elements under one group and reported as an approximation.
        let mut stacked = p.clone();
        let style = stacked
            .styles
            .iter_mut()
            .find(|s| s.style_id == under.style_id)
            .unwrap();
        style.items = vec![
            AppearanceItem::Fill(Paint::Solid(rgba(255, 0, 0, 1.0))),
            style.items[0].clone(),
            AppearanceItem::Fill(Paint::Solid(rgba(0, 255, 0, 1.0))),
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
        let Some(Paint::Linear(g)) = style_of(&p, "cycle").unwrap().fills().next() else {
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
            style_of(&p, "single").unwrap().fills().next(),
            Some(&Paint::Solid(rgba(0, 255, 0, 1.0)))
        );
        assert_eq!(
            style_of(&p, "missing").unwrap().fills().next(),
            Some(&Paint::Solid(rgba(0, 255, 0, 1.0)))
        );
        assert!(p.has_loss_code("unsupported_paint_server"));

        // objectBoundingBox on a zero-height bounding box paints nothing, as SVG defines.
        assert!(style_of(&p, "flat").is_none());
        assert!(p.has_loss_code("gradient_degenerate_bbox"));
    });
}
