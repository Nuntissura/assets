//! Lane SMB-COMPOSITE-STROKE-GEOMETRY slice 3: the pen joins network edges (STU-VEC-007 "connect to
//! any existing network anchor, not only endpoints"; STU-VEC-008).
use hsk_studio_nib::network::{NetworkError, NetworkHit, NetworkRegion, NetworkSegment, NetworkVertex, VectorNetwork};
use hsk_studio_nib::{FillRule, PathPoint};

fn p(x: f64, y: f64) -> PathPoint {
    PathPoint::new(x, y)
}

fn square() -> VectorNetwork {
    VectorNetwork {
        vertices: vec![NetworkVertex::at(0.0, 0.0), NetworkVertex::at(10.0, 0.0), NetworkVertex::at(10.0, 10.0), NetworkVertex::at(0.0, 10.0)],
        segments: vec![NetworkSegment::line(0, 1), NetworkSegment::line(1, 2), NetworkSegment::line(2, 3), NetworkSegment::line(3, 0)],
        regions: vec![NetworkRegion { winding_rule: FillRule::NonZero, loops: vec![vec![0, 1, 2, 3]], fills: true, fill_style_id: None }],
    }
}

#[test]
fn pen_clicks_join_edges_and_vertices() {
    let net = square();
    // A click on the bottom edge splits it: a new vertex, the region loop lengthened in walk order.
    assert!(matches!(net.hit(p(5.0, 0.2), 0.5), Some(NetworkHit::Segment { segment: 0, .. })));
    assert_eq!(net.hit(p(0.1, 0.1), 0.5), Some(NetworkHit::Vertex(0)));
    let (a, v1) = net.pen(None, p(5.0, 0.0), None, 0.5).unwrap();
    assert_eq!((v1, a.vertices.len(), a.segments.len()), (4, 5, 5));
    assert!((a.vertices[4].x - 5.0).abs() < 1e-6 && a.vertices[4].y == 0.0, "{:?}", a.vertices[4]);
    assert_eq!(a.regions[0].loops[0], vec![0, 4, 1, 2, 3]);
    a.validate().unwrap();
    // The next click lands on the top edge: split and joined to the previous pen vertex.
    let (b, v2) = a.pen(Some(v1), p(5.0, 10.0), None, 0.5).unwrap();
    assert_eq!((v2, b.segments.len()), (5, 7));
    assert_eq!(*b.segments.last().unwrap(), NetworkSegment::line(4, 5));
    let faces = b.detect_regions(true).unwrap();
    assert_eq!(faces.len(), 2, "the pen stroke divides the square");
    // A click on an existing vertex joins it (vertex 3 now meets three segments); a drag sets the tangent.
    let (c, v3) = b.pen(Some(v2), p(0.2, 9.9), Some(p(-2.0, 0.0)), 0.5).unwrap();
    assert_eq!((v3, c.vertices.len()), (3, 6));
    let last = *c.segments.last().unwrap();
    assert_eq!((last.start, last.end, last.tangent_start, last.tangent_end), (5, 3, Some(p(-2.0, 0.0)), Some(p(2.0, 0.0))));
    assert_eq!(c.segments.iter().filter(|s| s.start == 3 || s.end == 3).count(), 3);
    // A free click adds a free vertex; clicking the same vertex again adds no segment.
    let (d, v4) = c.pen(Some(v3), p(30.0, 30.0), None, 0.5).unwrap();
    assert_eq!((v4, d.segments.len()), (6, c.segments.len() + 1));
    let (e, v5) = d.pen(Some(v4), p(30.0, 30.0), None, 0.5).unwrap();
    assert_eq!((v5, e.segments.len()), (6, d.segments.len()));
    // A curved edge splits exactly by de Casteljau: the halves meet at the clicked point.
    let curved = VectorNetwork {
        vertices: vec![NetworkVertex::at(0.0, 0.0), NetworkVertex::at(10.0, 0.0)],
        segments: vec![NetworkSegment { start: 0, end: 1, tangent_start: Some(p(0.0, 10.0)), tangent_end: Some(p(0.0, 10.0)) }],
        regions: vec![],
    };
    let (s, mid) = curved.split_segment(0, 0.5).unwrap();
    assert_eq!(s.vertices[mid], NetworkVertex::at(5.0, 7.5));
    assert_eq!(s.segments[0].tangent_end, Some(p(-2.5, 0.0)));
    assert_eq!(s.segments[1].tangent_start, Some(p(2.5, 0.0)));
    // Typed refusals.
    assert_eq!(net.pen(Some(9), p(1.0, 1.0), None, 0.5).unwrap_err(), NetworkError::IndexOutOfBounds);
    assert_eq!(net.pen(None, p(f64::NAN, 1.0), None, 0.5).unwrap_err(), NetworkError::NonFinite);
    assert_eq!(net.split_segment(0, 0.0).unwrap_err(), NetworkError::OutOfRange);
    assert_eq!(net.split_segment(7, 0.5).unwrap_err(), NetworkError::IndexOutOfBounds);
}
