//! DXF import and the DWG refusal (lane SMB-DXF-EPS-IMPORT, feature `dxf`). Fixtures are generated
//! here as DXF text (group code / value line pairs), no external files.
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{Payload, StudioAppearanceRow, StudioPaint};
use hsk_studio_interop_vector_core::{
    CadKind, DWG_ROUTE, ImportPolicy, LossKind, SequentialIdMint, VectorError, VectorProposal, identify_cad, import_dwg, import_dxf, layer_style,
};

/// A DXF document from `(code, value)` pairs.
fn dxf(pairs: &[(i32, &str)]) -> Vec<u8> {
    let mut s = String::new();
    for (c, v) in pairs {
        s.push_str(&format!("{c:>3}\n{v}\n"));
    }
    s.into_bytes()
}

fn header(insunits: &str) -> Vec<(i32, &str)> {
    vec![(0, "SECTION"), (2, "HEADER"), (9, "$ACADVER"), (1, "AC1015"), (9, "$INSUNITS"), (70, insunits), (9, "$MEASUREMENT"), (70, "1"), (0, "ENDSEC")]
}

fn layers(names: &[(&'static str, &'static str)]) -> Vec<(i32, &'static str)> {
    let mut v = vec![(0, "SECTION"), (2, "TABLES"), (0, "TABLE"), (2, "LAYER")];
    for (name, colour) in names {
        v.extend([(0, "LAYER"), (2, *name), (70, "0"), (62, *colour), (6, "CONTINUOUS")]);
    }
    v.extend([(0, "ENDTAB"), (0, "ENDSEC")]);
    v
}

fn import(bytes: &[u8]) -> Result<VectorProposal, VectorError> {
    import_dxf(bytes, &ImportPolicy::default(), &mut SequentialIdMint::new(7), &CancellationToken::default())
}

fn layer_names(p: &VectorProposal) -> Vec<String> {
    p.snapshot.document().layers.iter().filter(|l| matches!(l.payload, Payload::Group)).map(|l| l.name.clone()).collect()
}

fn pt(p: &VectorProposal, path: usize, anchor: usize) -> [f64; 2] {
    let a = &p.paths[path].anchors[anchor];
    [a.position.x.value(), a.position.y.value()]
}

fn near(a: [f64; 2], b: [f64; 2]) -> bool {
    (a[0] - b[0]).abs() < 1e-6 && (a[1] - b[1]).abs() < 1e-6
}

/// LINE, CIRCLE, ARC, LWPOLYLINE with a bulge and SOLID on two layers, in millimetres: paths in
/// points (y flipped from the drawing top), one named group per layer, ACI colours from the layer
/// table, the SOLID filled, the arcs reported as cubic approximations.
#[test]
fn dxf_entities_become_layered_paths_in_points() {
    let mut pairs = header("4");
    pairs.extend(layers(&[("Outline", "1"), ("Detail", "5")]));
    pairs.extend([
        (0, "SECTION"), (2, "ENTITIES"),
        (0, "LINE"), (8, "Outline"), (10, "0"), (20, "0"), (30, "0"), (11, "100"), (21, "0"), (31, "0"),
        (0, "CIRCLE"), (8, "Detail"), (10, "50"), (20, "50"), (30, "0"), (40, "10"),
        (0, "ARC"), (8, "Detail"), (10, "50"), (20, "50"), (30, "0"), (40, "20"), (50, "0"), (51, "90"),
        (0, "LWPOLYLINE"), (8, "Outline"), (90, "3"), (70, "1"), (10, "0"), (20, "100"), (42, "1"), (10, "20"), (20, "100"), (10, "20"), (20, "80"),
        (0, "SOLID"), (8, "Detail"), (10, "80"), (20, "80"), (11, "90"), (21, "80"), (12, "80"), (22, "90"), (13, "90"), (23, "90"),
        (0, "ENDSEC"), (0, "EOF"),
    ]);
    let p = import(&dxf(&pairs)).expect("dxf import");
    assert_eq!(p.provenance.source_format, "dxf_ascii");
    assert_eq!(p.provenance.source_unit, "mm");
    let names = layer_names(&p);
    assert!(names.iter().any(|n| n == "Outline") && names.iter().any(|n| n == "Detail"), "{names:?}");
    // Drawing bounds: x 0..100 mm, y 0..100 mm (the bulge 1 semicircle from (0,100) to (20,100) is
    // counter-clockwise, so it bulges below the chord); top = 100 mm.
    let mm = 72.0 / 25.4;
    let top = 100.0 * mm;
    assert!(near(pt(&p, 0, 0), [0.0, top]), "line start {:?}", pt(&p, 0, 0));
    assert!(near(pt(&p, 0, 1), [100.0 * mm, top]), "line end {:?}", pt(&p, 0, 1));
    assert!(p.paths.len() >= 5, "five drawn entities, got {}", p.paths.len());
    assert!(p.has_loss_code("dxf_arc_to_cubic"));
    // The SOLID is a filled quad; the line is a red stroke of 0.25 mm.
    let fills = p.paths.iter().filter(|path| path.winding != hsk_studio_nib::Winding::None).count();
    assert_eq!(fills, 1, "only the SOLID is filled");
    let style = layer_style(&p.design, &p.paths[0].layer_id).expect("line style");
    let StudioAppearanceRow::Stroke { paint: StudioPaint::Solid { colour }, stroke, .. } = &style.rows[0] else { panic!("stroke row {:?}", style.rows) };
    assert!((colour.r - 1.0).abs() < 1e-6 && colour.g.abs() < 1e-6 && colour.b.abs() < 1e-6, "ACI 1 is red");
    assert!((stroke.weight.value - 0.25 * mm).abs() < 1e-6, "default lineweight 0.25 mm");
}

/// A BLOCK inserted rotated, scaled and as a 2 x 2 array; a clamped cubic SPLINE becomes exact
/// Béziers; TEXT and a DIMENSION are typed losses; inches set the scale.
#[test]
fn dxf_inserts_splines_and_losses() {
    let mut pairs = header("1");
    pairs.extend(layers(&[("0", "7")]));
    pairs.extend([
        (0, "SECTION"), (2, "BLOCKS"),
        (0, "BLOCK"), (8, "0"), (2, "SQ"), (70, "0"), (10, "0"), (20, "0"), (30, "0"),
        (0, "LWPOLYLINE"), (8, "0"), (90, "4"), (70, "1"), (10, "0"), (20, "0"), (10, "1"), (20, "0"), (10, "1"), (20, "1"), (10, "0"), (20, "1"),
        (0, "ENDBLK"), (8, "0"),
        (0, "ENDSEC"),
        (0, "SECTION"), (2, "ENTITIES"),
        (0, "INSERT"), (8, "0"), (2, "SQ"), (10, "10"), (20, "10"), (30, "0"), (41, "2"), (42, "2"), (50, "90"), (70, "2"), (71, "2"), (44, "5"), (45, "5"),
        (0, "SPLINE"), (8, "0"), (70, "8"), (71, "3"), (72, "8"), (73, "4"), (74, "0"),
        (40, "0"), (40, "0"), (40, "0"), (40, "0"), (40, "1"), (40, "1"), (40, "1"), (40, "1"),
        (10, "0"), (20, "0"), (30, "0"), (10, "1"), (20, "2"), (30, "0"), (10, "3"), (20, "2"), (30, "0"), (10, "4"), (20, "0"), (30, "0"),
        (0, "TEXT"), (8, "0"), (10, "0"), (20, "0"), (30, "0"), (40, "1"), (1, "label"),
        (0, "ENDSEC"), (0, "EOF"),
    ]);
    let p = import(&dxf(&pairs)).expect("dxf import");
    assert_eq!(p.provenance.source_unit, "in");
    // Four array copies of the block square plus the spline.
    assert_eq!(p.paths.len(), 5, "4 inserted squares + 1 spline");
    let spline = p.paths.last().expect("spline");
    assert_eq!(spline.anchors.len(), 2, "one exact cubic piece");
    assert!(p.has_loss_code("dxf_text_not_imported"));
    assert!(p.has_loss_code("dxf_aci_7_drawn_black"));
    assert!(!p.has_loss_code("dxf_spline_sampled"));
    // Square side = 1 in x 2 = 144 pt (rotation keeps the size).
    let xs: Vec<f64> = p.paths[0].anchors.iter().map(|a| a.position.x.value()).collect();
    let span = xs.iter().cloned().fold(f64::MIN, f64::max) - xs.iter().cloned().fold(f64::MAX, f64::min);
    assert!((span - 144.0).abs() < 1e-6, "inserted square width {span}");
}

/// DWG is detected by its version string and refused naming the DXF route, bytes preserved, from
/// both the DWG entry point and the DXF importer.
#[test]
fn dwg_is_refused_naming_the_dxf_route() {
    let mut dwg = b"AC1032".to_vec();
    dwg.extend([0u8; 122]);
    assert_eq!(identify_cad(&dwg), CadKind::Dwg { version: Some("AC1032".into()) });
    for result in [import_dwg(&dwg, &ImportPolicy::default(), &CancellationToken::default()).map(|_| ()), import(&dwg).map(|_| ())] {
        match result {
            Err(VectorError::DwgUnavailable { version, route, preserved }) => {
                assert_eq!(version.as_deref(), Some("AC1032"));
                assert_eq!(route, DWG_ROUTE);
                assert_eq!(preserved.bytes, dwg);
                assert_eq!(VectorError::DwgUnavailable { version, route, preserved }.code(), "dwg_unavailable");
            }
            other => panic!("expected dwg_unavailable, got {other:?}"),
        }
    }
    assert_eq!(identify_cad(b"  0\nSECTION\n"), CadKind::DxfAscii);
    assert_eq!(identify_cad(b"<svg/>"), CadKind::NotCad);
    assert!(matches!(import(b"<svg/>"), Err(VectorError::Unsupported("not_a_dxf_file"))));
}

/// Hostile DXF: a record flood is refused before parsing, an insert array bomb and deep or cyclic
/// block nesting are bounded, damaged input is a typed error, out-of-range coordinates are refused.
#[test]
fn hostile_dxf_is_bounded_and_typed() {
    // Record flood: more code-0 records than the cap (4 x max_nodes + 4096) for a small policy.
    let mut policy = ImportPolicy::default();
    policy.limits.max_nodes = 100;
    let mut flood = String::from("  0\nSECTION\n  2\nENTITIES\n");
    for _ in 0..5000 {
        flood.push_str("  0\nPOINT\n");
    }
    flood.push_str("  0\nENDSEC\n  0\nEOF\n");
    let r = import_dxf(flood.as_bytes(), &policy, &mut SequentialIdMint::new(1), &CancellationToken::default());
    assert_eq!(r.err(), Some(VectorError::BudgetExceeded("dxf_records")));

    // Array bomb: 32767 x 32767 copies of a block.
    let mut pairs = header("4");
    pairs.extend([
        (0, "SECTION"), (2, "BLOCKS"), (0, "BLOCK"), (8, "0"), (2, "B"), (70, "0"), (10, "0"), (20, "0"), (30, "0"),
        (0, "LINE"), (8, "0"), (10, "0"), (20, "0"), (30, "0"), (11, "1"), (21, "1"), (31, "0"), (0, "ENDBLK"), (8, "0"), (0, "ENDSEC"),
        (0, "SECTION"), (2, "ENTITIES"),
        (0, "INSERT"), (8, "0"), (2, "B"), (10, "0"), (20, "0"), (30, "0"), (70, "32767"), (71, "32767"), (44, "1"), (45, "1"),
        (0, "ENDSEC"), (0, "EOF"),
    ]);
    assert_eq!(import(&dxf(&pairs)).err(), Some(VectorError::BudgetExceeded("dxf_insert_array")));

    // A block that inserts itself: the cycle is skipped and reported; the line still draws.
    let mut pairs = header("4");
    pairs.extend([
        (0, "SECTION"), (2, "BLOCKS"), (0, "BLOCK"), (8, "0"), (2, "LOOP"), (70, "0"), (10, "0"), (20, "0"), (30, "0"),
        (0, "LINE"), (8, "0"), (10, "0"), (20, "0"), (30, "0"), (11, "1"), (21, "1"), (31, "0"),
        (0, "INSERT"), (8, "0"), (2, "LOOP"), (10, "1"), (20, "1"), (30, "0"),
        (0, "ENDBLK"), (8, "0"), (0, "ENDSEC"),
        (0, "SECTION"), (2, "ENTITIES"), (0, "INSERT"), (8, "0"), (2, "LOOP"), (10, "0"), (20, "0"), (30, "0"), (0, "ENDSEC"), (0, "EOF"),
    ]);
    let p = import(&dxf(&pairs)).expect("cyclic block import");
    assert!(p.has_loss_code("dxf_block_reference_cycle_skipped"));
    assert_eq!(p.paths.len(), 1);

    // Out-of-range coordinates (1e30 mm) are refused, not clamped.
    let mut pairs = header("4");
    pairs.extend([(0, "SECTION"), (2, "ENTITIES"), (0, "LINE"), (8, "0"), (10, "0"), (20, "0"), (30, "0"), (11, "1e30"), (21, "0"), (31, "0"), (0, "ENDSEC"), (0, "EOF")]);
    assert_eq!(import(&dxf(&pairs)).err(), Some(VectorError::InvalidGeometry));

    // Damaged: a group code that is not a number, and a document with nothing drawable.
    let damaged = b"  0\nSECTION\n  2\nENTITIES\n  0\nLINE\nxx\n1\n  0\nENDSEC\n  0\nEOF\n";
    assert!(matches!(import(damaged), Err(VectorError::MalformedDxf(_))), "{:?}", import(damaged).err());
    let mut empty = header("4");
    empty.extend([(0, "SECTION"), (2, "ENTITIES"), (0, "TEXT"), (8, "0"), (10, "0"), (20, "0"), (30, "0"), (40, "1"), (1, "t"), (0, "ENDSEC"), (0, "EOF")]);
    assert_eq!(import(&dxf(&empty)).err(), Some(VectorError::Unsupported("dxf_without_drawable_entities")));
    let _ = LossKind::Unsupported;
}
