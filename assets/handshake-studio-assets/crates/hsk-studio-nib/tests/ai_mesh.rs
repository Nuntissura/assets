//! Lane SMB-AI-TYPE-PATH-RECOLOR slice 3: gradient mesh geometry (STU-VEC-034, STU-COL-036).
//! Expected points and handles are hand-derived (straight mesh lines carry their handles at the
//! thirds). Illustrator mesh parity is UNVERIFIED (D5).
use hsk_studio_nib::mesh::*;
use hsk_studio_nib::{FillRule, OwnedPath, PathElement, PathPoint};

const RED: [f32; 4] = [1.0, 0.0, 0.0, 1.0];
const GREEN: [f32; 4] = [0.0, 1.0, 0.0, 1.0];

fn near(a: MeshPt, b: MeshPt) {
    assert!((a[0] - b[0]).abs() < 1e-9 && (a[1] - b[1]).abs() < 1e-9, "{a:?} != {b:?}");
}
fn path(points: &[[f64; 2]]) -> OwnedPath {
    let mut elements = vec![PathElement::MoveTo(PathPoint::new(points[0][0], points[0][1]))];
    elements.extend(points[1..].iter().map(|p| PathElement::LineTo(PathPoint::new(p[0], p[1]))));
    elements.push(PathElement::Close);
    OwnedPath { elements, fill_rule: FillRule::NonZero }
}

#[test]
fn ai_mesh_grid_from_path_and_line_edits() {
    let lim = MeshLimits::default();
    // 2 x 3 grid over 30 x 20: point (r, c) at (10 c, 10 r); a horizontal line's handles at the thirds.
    let g = GradientMesh::grid([0.0, 0.0, 30.0, 20.0], 2, 3, RED, &lim).unwrap();
    assert_eq!((g.points.len(), g.h_handles.len(), g.v_handles.len()), (12, 9, 8));
    near(g.point(2, 3).unwrap().pos, [30.0, 20.0]);
    near(g.point(1, 2).unwrap().pos, [20.0, 10.0]);
    near(g.h_handles[0][0], [10.0 / 3.0, 0.0]);
    near(g.h_handles[0][1], [20.0 / 3.0, 0.0]);
    near(g.evaluate(1, 1, 0.5, 0.5).unwrap().0, [15.0, 15.0]);
    // A closed 4-segment path is the patch boundary: the rectangle path gives the grid.
    let r = GradientMesh::from_path(&path(&[[0.0, 0.0], [30.0, 0.0], [30.0, 20.0], [0.0, 20.0]]), 2, 3, RED, &lim).unwrap();
    for (a, b) in r.points.iter().zip(&g.points) {
        near(a.pos, b.pos);
    }
    for (a, b) in r.h_handles.iter().chain(&r.v_handles).zip(g.h_handles.iter().chain(&g.v_handles)) {
        near(a[0], b[0]);
        near(a[1], b[1]);
    }
    // Any other closed path is cut at quarter arc lengths: a 20 x 20 square with each side split at
    // its midpoint (8 segments of 10) is cut at its corners, so the mesh is the 20 x 20 grid.
    let oct = GradientMesh::from_path(&path(&[[0.0, 0.0], [10.0, 0.0], [20.0, 0.0], [20.0, 10.0], [20.0, 20.0], [10.0, 20.0], [0.0, 20.0], [0.0, 10.0]]), 2, 2, RED, &lim).unwrap();
    near(oct.point(0, 0).unwrap().pos, [0.0, 0.0]);
    near(oct.point(0, 2).unwrap().pos, [20.0, 0.0]);
    near(oct.point(2, 2).unwrap().pos, [20.0, 20.0]);
    near(oct.point(1, 1).unwrap().pos, [10.0, 10.0]);

    // Insert a column at a third of a 30 x 30 patch: new points (10, 0) and (10, 30), colour
    // interpolated a third of the way from red to green; the new line straight with handles at its thirds.
    let mut m = GradientMesh::grid([0.0, 0.0, 30.0, 30.0], 1, 1, RED, &lim).unwrap();
    m.set_colour(0, 1, GREEN).unwrap();
    m.insert_column(0, 1.0 / 3.0, &lim).unwrap();
    assert_eq!((m.columns, m.points.len()), (2, 6));
    near(m.point(0, 1).unwrap().pos, [10.0, 0.0]);
    near(m.point(1, 1).unwrap().pos, [10.0, 30.0]);
    let c = m.point(0, 1).unwrap().colour;
    assert!((c[0] - 2.0 / 3.0).abs() < 1e-6 && (c[1] - 1.0 / 3.0).abs() < 1e-6, "{c:?}");
    near(m.v_handles[1][0], [10.0, 10.0]);
    near(m.v_handles[1][1], [10.0, 20.0]);
    // Removing it merges the two edges, keeping the outer handles at twice their length:
    // [(0,0) + 2 (10/3, 0), (30,0) + 2 ((10 + 40/3, 0) - (30, 0))] = [(20/3, 0), (50/3, 0)].
    m.remove_column(1, &lim).unwrap();
    assert_eq!((m.columns, m.points.len()), (1, 4));
    near(m.h_handles[0][0], [20.0 / 3.0, 0.0]);
    near(m.h_handles[0][1], [50.0 / 3.0, 0.0]);
    // Rows go through the transposed mesh.
    let mut m = GradientMesh::grid([0.0, 0.0, 30.0, 30.0], 1, 1, RED, &lim).unwrap();
    m.insert_row(0, 0.5, &lim).unwrap();
    assert_eq!((m.rows, m.columns), (2, 1));
    near(m.point(1, 0).unwrap().pos, [0.0, 15.0]);
    near(m.point(1, 1).unwrap().pos, [30.0, 15.0]);
    m.remove_row(1, &lim).unwrap();
    assert_eq!((m.rows, m.points.len()), (1, 4));
    // Moving a point carries its handles.
    let mut m = GradientMesh::grid([0.0, 0.0, 30.0, 30.0], 1, 1, RED, &lim).unwrap();
    m.move_point(0, 0, [1.0, 2.0]).unwrap();
    near(m.point(0, 0).unwrap().pos, [1.0, 2.0]);
    near(m.h_handles[0][0], [11.0, 2.0]);
    near(m.v_handles[0][0], [1.0, 12.0]);
}

#[test]
fn ai_mesh_tessellation_and_hostile_input() {
    let lim = MeshLimits::default();
    // A 10 x 10 patch with sub-quads of at most 5 units: 2 x 2 sub-quads, 9 vertices, 8 triangles.
    let g = GradientMesh::grid([0.0, 0.0, 10.0, 10.0], 1, 1, RED, &lim).unwrap();
    let t = g.tessellate(5.0, &lim).unwrap();
    assert_eq!((t.vertices.len(), t.triangles.len(), t.patch_colours.len()), (9, 8, 1));
    near(t.vertices[4].pos, [5.0, 5.0]);
    assert_eq!(t.vertices[4].uv, [0.5, 0.5]);
    assert_eq!(t.colour(0, 0.5, 0.5), RED);
    // Grid size, patch count and triangle caps, checked before allocation.
    assert_eq!(GradientMesh::grid([0.0, 0.0, 1.0, 1.0], 0, 1, RED, &lim), Err(MeshError::InvalidInput("grid")));
    assert_eq!(GradientMesh::grid([0.0, 0.0, 1.0, 1.0], 257, 1, RED, &lim), Err(MeshError::OverLimit("max_lines")));
    assert_eq!(GradientMesh::grid([0.0, 0.0, 1.0, 1.0], 200, 200, RED, &lim), Err(MeshError::OverLimit("max_patches")));
    assert_eq!(g.tessellate(0.001, &MeshLimits { max_triangles: 100, ..lim }), Err(MeshError::OverLimit("max_triangles")));
    assert_eq!(g.tessellate(0.0, &lim), Err(MeshError::InvalidInput("max_step")));
    // Degenerate and non-finite geometry, bad colours, inconsistent arrays.
    assert_eq!(GradientMesh::grid([0.0, 0.0, 0.0, 1.0], 1, 1, RED, &lim), Err(MeshError::DegeneratePath));
    assert_eq!(GradientMesh::from_path(&path(&[[5.0, 5.0], [5.0, 5.0], [5.0, 5.0]]), 1, 1, RED, &lim), Err(MeshError::DegeneratePath));
    assert_eq!(GradientMesh::from_path(&path(&[[0.0, 0.0], [10.0, 0.0]]), 1, 1, RED, &lim), Err(MeshError::DegeneratePath));
    assert_eq!(GradientMesh::from_path(&path(&[[0.0, 0.0], [10.0, 0.0], [10.0, 10.0]]), 1, 1, [2.0, 0.0, 0.0, 1.0], &lim), Err(MeshError::InvalidInput("colour")));
    let mut bad = g.clone();
    bad.points[0].pos = [f64::NAN, 0.0];
    assert_eq!(bad.validate(&lim), Err(MeshError::InvalidInput("point")));
    let mut bad = g.clone();
    bad.h_handles.pop();
    assert_eq!(bad.validate(&lim), Err(MeshError::InvalidInput("array_sizes")));
    let mut m = g.clone();
    assert_eq!(m.insert_column(0, 1.0, &lim), Err(MeshError::InvalidInput("insert_column")));
    assert_eq!(m.remove_column(0, &lim), Err(MeshError::InvalidInput("remove_column")));
    assert_eq!(m.set_colour(9, 0, RED), Err(MeshError::InvalidInput("point_index")));
    assert_eq!(m.move_point(0, 0, [f64::INFINITY, 0.0]), Err(MeshError::InvalidInput("move")));
}
