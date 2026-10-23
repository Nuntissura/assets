//! Lane SMB-ILLUSTRATOR-PEN-2 slice 5: `StudioVectorNetwork` (STU-VEC-001, STU-VEC-004, STU-VEC-108)
//! against hand-built topology.
use hsk_studio_nib::anchor_path::{AnchorPath, Contour, EditAnchor};
use hsk_studio_nib::network::{MAX_NETWORK_ELEMENTS, NetworkError, NetworkRegion, NetworkSegment, NetworkVertex, VectorNetwork};
use hsk_studio_nib::shape::{RectSpec, rectangle};
use hsk_studio_nib::{FillRule, Mirroring, PathPoint};

fn p(x: f64, y: f64) -> PathPoint {
    PathPoint::new(x, y)
}

fn shoelace(c: &Contour) -> f64 {
    let n = c.anchors.len();
    (0..n).map(|i| {
        let (a, b) = (c.anchors[i].position, c.anchors[(i + 1) % n].position);
        a.x * b.y - b.x * a.y
    }).sum::<f64>().abs() / 2.0
}

/// Square A(0,0) B(10,0) C(10,10) D(0,10) with the diagonal A-C: vertices A and C join three segments.
fn theta() -> VectorNetwork {
    VectorNetwork {
        vertices: vec![NetworkVertex::at(0.0, 0.0), NetworkVertex::at(10.0, 0.0), NetworkVertex::at(10.0, 10.0), NetworkVertex::at(0.0, 10.0)],
        segments: vec![NetworkSegment::line(0, 1), NetworkSegment::line(1, 2), NetworkSegment::line(2, 3), NetworkSegment::line(3, 0), NetworkSegment::line(0, 2)],
        regions: vec![],
    }
}

#[test]
fn path_promotes_losslessly_and_flattens_back() {
    let square = rectangle(&RectSpec::plain(0.0, 0.0, 10.0, 10.0)).unwrap().0;
    let mut a = EditAnchor::corner(20.0, 0.0);
    a.outgoing = p(0.0, 5.0);
    let mut b = EditAnchor { mirroring: Mirroring::AngleAndLength, ..EditAnchor::corner(30.0, 5.0) };
    b.incoming = p(-3.0, 0.0);
    b.outgoing = p(3.0, 0.0);
    let arc = Contour { anchors: vec![a, b, EditAnchor::corner(40.0, 0.0)], closed: false };
    let path = AnchorPath { contours: vec![square.contours[0].clone(), arc.clone()], fill_rule: FillRule::EvenOdd };
    let net = VectorNetwork::from_path(&path).unwrap();
    assert_eq!((net.vertices.len(), net.segments.len(), net.regions.len()), (7, 6, 1));
    assert_eq!(net.vertices[5].handle_mirroring, Some(Mirroring::AngleAndLength));
    assert_eq!(net.segments[4].tangent_start, Some(p(0.0, 5.0)));
    assert_eq!(net.segments[4].tangent_end, Some(p(-3.0, 0.0)));
    assert_eq!(net.segments[0].tangent_start, None, "straight segments carry no tangents");
    let region = &net.regions[0];
    assert_eq!((region.winding_rule, region.fills, region.loops.clone()), (FillRule::EvenOdd, true, vec![vec![0, 1, 2, 3]]));
    // fill_geometry: the closed contour, exactly.
    let fill = net.fill_geometry().unwrap();
    assert_eq!(fill.len(), 1);
    assert_eq!(fill[0].contours, vec![square.contours[0].clone()]);
    assert_eq!(fill[0].fill_rule, FillRule::EvenOdd);
    // stroke_geometry: the square as a closed chain and the arc as an open chain, handles kept.
    let stroke = net.stroke_geometry().unwrap();
    assert_eq!(stroke.contours.len(), 2);
    let open = stroke.contours.iter().find(|c| !c.closed).unwrap();
    assert_eq!(open.anchors.iter().map(|a| (a.position, a.incoming, a.outgoing)).collect::<Vec<_>>(), arc.anchors.iter().map(|a| (a.position, a.incoming, a.outgoing)).collect::<Vec<_>>());
    let closed = stroke.contours.iter().find(|c| c.closed).unwrap();
    assert_eq!(closed.anchors.len(), 4);
    assert_eq!(shoelace(closed), 100.0);
}

#[test]
fn junction_regions_paint_bucket_and_stroke_chains() {
    let mut net = theta();
    // Faces of the embedding: the two triangles (the unbounded face is dropped).
    let faces = net.detect_regions(true).unwrap();
    assert_eq!(faces.len(), 2);
    let mut loops: Vec<Vec<usize>> = faces.iter().map(|f| {
        let mut l = f.loops[0].clone();
        l.sort_unstable();
        l
    }).collect();
    loops.sort();
    assert_eq!(loops, vec![vec![0, 1, 4], vec![2, 3, 4]]);
    net.regions = faces;
    net.validate().unwrap();
    let fill = net.fill_geometry().unwrap();
    assert_eq!(fill.len(), 2);
    for f in &fill {
        assert_eq!(shoelace(&f.contours[0]), 50.0);
    }
    // Paint bucket: clear one region; topology untouched.
    let cleared = net.paint_bucket(0, false).unwrap();
    assert_eq!((cleared.vertices.clone(), cleared.segments.clone()), (net.vertices.clone(), net.segments.clone()));
    assert_eq!(cleared.fill_geometry().unwrap().len(), 1);
    assert_eq!(cleared.paint_bucket(0, true).unwrap(), net);
    // Stroke geometry draws every segment exactly once.
    let stroke = net.stroke_geometry().unwrap();
    let drawn: usize = stroke.contours.iter().map(|c| c.segment_count()).sum();
    assert_eq!(drawn, 5);
    // A curved segment between two vertices plus a straight one bounds one face (a lens).
    let lens = VectorNetwork {
        vertices: vec![NetworkVertex::at(0.0, 0.0), NetworkVertex::at(10.0, 0.0)],
        segments: vec![NetworkSegment::line(0, 1), NetworkSegment { start: 1, end: 0, tangent_start: Some(p(0.0, 5.0)), tangent_end: Some(p(0.0, 5.0)) }],
        regions: vec![],
    };
    assert_eq!(lens.detect_regions(false).unwrap().len(), 1);
}

#[test]
fn hostile_networks_are_refused_typed() {
    let mut bad = theta();
    bad.segments.push(NetworkSegment::line(0, 9));
    assert_eq!(bad.validate(), Err(NetworkError::IndexOutOfBounds), "never clamped");
    let mut bad = theta();
    bad.regions.push(NetworkRegion { winding_rule: FillRule::NonZero, loops: vec![vec![0, 1, 99]], fills: true, fill_style_id: None });
    assert_eq!(bad.validate(), Err(NetworkError::IndexOutOfBounds));
    let mut bad = theta();
    bad.regions.push(NetworkRegion { winding_rule: FillRule::NonZero, loops: vec![vec![0, 2]], fills: true, fill_style_id: None });
    assert_eq!(bad.validate(), Err(NetworkError::OpenLoop));
    let mut bad = theta();
    bad.regions.push(NetworkRegion { winding_rule: FillRule::NonZero, loops: vec![], fills: true, fill_style_id: None });
    assert_eq!(bad.validate(), Err(NetworkError::InvalidRegion));
    let mut bad = theta();
    bad.regions.push(NetworkRegion { winding_rule: FillRule::NonZero, loops: vec![vec![0, 1, 4]], fills: true, fill_style_id: Some("no spaces".into()) });
    assert_eq!(bad.validate(), Err(NetworkError::InvalidRegion));
    let mut bad = theta();
    bad.vertices[0].x = f64::NAN;
    assert_eq!(bad.validate(), Err(NetworkError::NonFinite));
    let mut bad = theta();
    bad.vertices[1].corner_radius = Some(-1.0);
    assert_eq!(bad.validate(), Err(NetworkError::OutOfRange));
    let mut bad = theta();
    bad.segments.push(NetworkSegment::line(2, 2));
    assert_eq!(bad.validate(), Err(NetworkError::Degenerate));
    let huge = VectorNetwork { vertices: vec![NetworkVertex::at(0.0, 0.0); MAX_NETWORK_ELEMENTS + 1], segments: vec![], regions: vec![] };
    assert_eq!(huge.validate(), Err(NetworkError::BudgetExceeded));
    assert_eq!(huge.detect_regions(true), Err(NetworkError::BudgetExceeded));
    assert_eq!(theta().paint_bucket(3, true), Err(NetworkError::IndexOutOfBounds));
    assert_eq!(NetworkError::IndexOutOfBounds.code(), "index_out_of_bounds");
}
