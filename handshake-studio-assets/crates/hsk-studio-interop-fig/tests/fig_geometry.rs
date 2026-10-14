//! Vector geometry of the native mapping (lane SMB-INTEROP-FIG-GEOMETRY) on the three Operator
//! Figma fixtures: shapes become Nib paths on vector layers with every node transform baked in,
//! pinned loss counts, bounding boxes checked against the fixture PNG renders, and a package
//! round trip of the geometry.
use hsk_studio_accord::{CancellationToken, SCHEMA_STUDIO_COMPONENT_INSTANCE};
use hsk_studio_folio::{Budget, Payload, Resolution, Resolver, TileRef};
use hsk_studio_interop_fig::*;
use hsk_studio_nib::{OwnedPath, PathElement, PathPoint};
use std::collections::BTreeMap;

struct Host;
impl Resolver for Host {
    fn primitive(&self, _: &str, _: &str) -> Resolution {
        Resolution::Absent
    }
    fn profile(&self, _: &str) -> Resolution {
        Resolution::Absent
    }
    fn tile(&self, _: &TileRef) -> Resolution {
        Resolution::Absent
    }
    fn format_supported(&self, _: &str) -> bool {
        false
    }
    fn primitive_layer_binding(&self, kind: &str, schema: &str) -> bool {
        kind == "component_instance" && schema == SCHEMA_STUDIO_COMPONENT_INSTANCE
    }
}

fn fixture(name: &str) -> Vec<u8> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("fixture {name}: {e}"))
}

struct Mapped {
    doc: FigDocument,
    proposal: NativeProposal,
}

fn mapped(name: &str) -> Mapped {
    let bytes = fixture(name);
    let cancel = CancellationToken::default();
    let FigOutcome::Imported(import) = import_fig(
        &bytes,
        &Limits::default(),
        &VersionPolicy::observed(),
        None,
        &cancel,
    )
    .unwrap() else {
        panic!("{name} imports")
    };
    let doc = decode_document(&import, &Limits::default(), &cancel).unwrap();
    let cfg = NativeConfig {
        budget: Budget::default(),
        resolver: &Host,
    };
    let proposal = to_native(&doc, &cfg, &mut SequentialIdMint::new(0xF16), &cancel).unwrap();
    Mapped { doc, proposal }
}

fn loss_counts(p: &NativeProposal) -> BTreeMap<(String, &'static str), u32> {
    p.mapping_loss
        .iter()
        .map(|l| ((l.path.clone(), l.code), l.count))
        .collect()
}

/// `[min_x, min_y, max_x, max_y]` of a path (control points included: they bound the curve).
fn bbox(p: &OwnedPath) -> [f64; 4] {
    let mut b = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
    let mut put = |q: &PathPoint| {
        b = [b[0].min(q.x), b[1].min(q.y), b[2].max(q.x), b[3].max(q.y)];
    };
    for e in &p.elements {
        match e {
            PathElement::MoveTo(a) | PathElement::LineTo(a) => put(a),
            PathElement::QuadTo(a, c) => {
                put(a);
                put(c)
            }
            PathElement::CubicTo(a, c, d) => {
                put(a);
                put(c);
                put(d)
            }
            PathElement::Close => {}
        }
    }
    b
}

/// Union bbox of every path of the named layer.
fn layer_bbox(p: &NativeProposal, name: &str) -> [f64; 4] {
    let layer = p
        .snapshot
        .document()
        .layers
        .iter()
        .find(|l| l.name == name)
        .unwrap_or_else(|| panic!("layer {name}"));
    let Payload::Vector { path_ids } = &layer.payload else {
        panic!("{name} is not a vector layer")
    };
    assert!(!path_ids.is_empty(), "{name} lists paths");
    let mut u = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
    for id in path_ids {
        let path = p.paths.iter().find(|x| &x.path_id == id).expect("path");
        assert_eq!(path.layer_id, layer.layer_id);
        let b = bbox(&path.geometry);
        u = [
            u[0].min(b[0]),
            u[1].min(b[1]),
            u[2].max(b[2]),
            u[3].max(b[3]),
        ];
    }
    u
}

/// 8-bit RGB/RGBA non-interlaced PNG -> (width, height, rgba bytes).
fn decode_png(bytes: &[u8]) -> (usize, usize, Vec<u8>) {
    assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
    let (mut at, mut idat) = (8usize, Vec::new());
    let (mut w, mut h, mut ct) = (0usize, 0usize, 0u8);
    while at + 8 <= bytes.len() {
        let len = u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
        let kind = &bytes[at + 4..at + 8];
        let body = &bytes[at + 8..at + 8 + len];
        match kind {
            b"IHDR" => {
                w = u32::from_be_bytes(body[0..4].try_into().unwrap()) as usize;
                h = u32::from_be_bytes(body[4..8].try_into().unwrap()) as usize;
                assert_eq!((body[8], body[12]), (8, 0), "8-bit, not interlaced");
                ct = body[9];
            }
            b"IDAT" => idat.extend_from_slice(body),
            _ => {}
        }
        at += 12 + len;
    }
    let bpp = match ct {
        2 => 3,
        6 => 4,
        other => panic!("colour type {other}"),
    };
    let raw = miniz_oxide::inflate::decompress_to_vec_zlib(&idat).expect("inflate");
    let stride = w * bpp;
    let mut out = vec![0u8; h * stride];
    for y in 0..h {
        let f = raw[y * (stride + 1)];
        let line = &raw[y * (stride + 1) + 1..(y + 1) * (stride + 1)];
        for x in 0..stride {
            let a = if x >= bpp {
                out[y * stride + x - bpp]
            } else {
                0
            };
            let b = if y > 0 { out[(y - 1) * stride + x] } else { 0 };
            let c = if y > 0 && x >= bpp {
                out[(y - 1) * stride + x - bpp]
            } else {
                0
            };
            let pred = match f {
                0 => 0,
                1 => a,
                2 => b,
                3 => ((u16::from(a) + u16::from(b)) / 2) as u8,
                4 => {
                    let (pa, pb, pc) = (
                        (i32::from(b) - i32::from(c)).abs(),
                        (i32::from(a) - i32::from(c)).abs(),
                        (i32::from(a) + i32::from(b) - 2 * i32::from(c)).abs(),
                    );
                    if pa <= pb && pa <= pc {
                        a
                    } else if pb <= pc {
                        b
                    } else {
                        c
                    }
                }
                other => panic!("filter {other}"),
            };
            out[y * stride + x] = line[x].wrapping_add(pred);
        }
    }
    let mut rgba = Vec::with_capacity(w * h * 4);
    for px in out.chunks(bpp) {
        rgba.extend_from_slice(&[px[0], px[1], px[2], if bpp == 4 { px[3] } else { 255 }]);
    }
    (w, h, rgba)
}

/// Painted bounding box (pixels differing from the corner background by more than 24 in any
/// channel) inside `window` = `[x0, y0, x1, y1]`; `None` when nothing is painted there.
fn painted(png: &(usize, usize, Vec<u8>), window: [f64; 4]) -> Option<[f64; 4]> {
    let (w, h, px) = png;
    let bg = [px[0], px[1], px[2]];
    let mut b = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
    let clamp = |v: f64, max: usize| v.max(0.0).min(max as f64) as usize;
    for y in clamp(window[1], *h)..clamp(window[3], *h) {
        for x in clamp(window[0], *w)..clamp(window[2], *w) {
            let o = (y * w + x) * 4;
            if (0..3).any(|c| px[o + c].abs_diff(bg[c]) > 24) {
                b = [
                    b[0].min(x as f64),
                    b[1].min(y as f64),
                    b[2].max(x as f64 + 1.0),
                    b[3].max(y as f64 + 1.0),
                ];
            }
        }
    }
    (b[0] <= b[2]).then_some(b)
}

fn near(a: [f64; 4], b: [f64; 4], tol: f64) -> bool {
    a.iter().zip(&b).all(|(x, y)| (x - y).abs() <= tol)
}

#[test]
fn real_geometry_losses_shrink_and_are_pinned() {
    // Per fixture: (nodes, native.geometry, native.stroke_geometry, native.transform, native.size,
    // native.corner_radii, paths). Before (bc78302) native.geometry equalled the number of nodes
    // with geometry (14 / 10 / 17) and every shape node added native.transform + native.size and
    // a corner_radii / arc / star / vector_network loss (basic-shapes: 12 shapes, 0 paths).
    let pins: [(&str, u32, u32, u32, u32, u32, usize); 3] = [
        ("fig-basic-shapes.fig", 2, 3, 1, 3, 0, 12),
        ("fig-components-autolayout.fig", 8, 0, 6, 7, 6, 2),
        ("fig-prototype-pages.fig", 14, 1, 14, 12, 1, 3),
    ];
    for (name, geometry, stroke, transform, size, radii, paths) in pins {
        let m = mapped(name);
        let c = loss_counts(&m.proposal);
        let get = |p: &str| {
            c.iter()
                .filter(|((path, _), _)| path == p)
                .map(|(_, n)| *n)
                .sum::<u32>()
        };
        let got = (
            get("native.geometry"),
            get("native.stroke_geometry"),
            get("native.transform"),
            get("native.size"),
            get("native.corner_radii"),
            m.proposal.paths.len(),
        );
        assert_eq!(
            got,
            (geometry, stroke, transform, size, radii, paths),
            "{name}"
        );
        for gone in ["native.arc", "native.star", "native.vector_network"] {
            assert_eq!(get(gone), 0, "{name} {gone}");
        }
        // Every path belongs to a vector layer that lists it; no path is orphaned.
        let doc = m.proposal.snapshot.document();
        for p in &m.proposal.paths {
            let l = doc
                .layers
                .iter()
                .find(|l| l.layer_id == p.layer_id)
                .unwrap();
            assert!(
                matches!(&l.payload, Payload::Vector { path_ids } if path_ids.contains(&p.path_id))
            );
        }
    }
}

#[test]
fn real_geometry_matches_png_positions() {
    let m = mapped("fig-basic-shapes.fig");
    let png = decode_png(&fixture("fig-basic-shapes.png"));
    assert_eq!((png.0, png.1), (900, 640));
    // The artboard sits at (100,100) of the canvas; the PNG is the artboard at 1x.
    let artboard = m.proposal.snapshot.document().artboards[0].bounds.clone();
    let (ox, oy) = (artboard.x.value, artboard.y.value);
    assert_eq!((ox, oy), (100.0, 100.0));
    let local = |b: [f64; 4]| [b[0] - ox, b[1] - oy, b[2] - ox, b[3] - oy];
    let grow = |b: [f64; 4]| [b[0] - 12.0, b[1] - 12.0, b[2] + 12.0, b[3] + 12.0];
    // Exact sizes of the plain shapes (transform chain baked: node offset + artboard offset).
    assert!(near(
        layer_bbox(&m.proposal, "ellipse-gradient"),
        [350.0, 190.0, 500.0, 300.0],
        1e-6
    ));
    assert!(near(
        layer_bbox(&m.proposal, "rounded-solid-shadow"),
        [140.0, 190.0, 310.0, 300.0],
        1e-6
    ));
    // Painted bounds in the render agree with the geometry within stroke width and AA.
    for (name, tol) in [("ellipse-gradient", 2.0), ("star-half-opacity", 3.0)] {
        let g = local(layer_bbox(&m.proposal, name));
        let p = painted(&png, grow(g)).unwrap_or_else(|| panic!("{name}: nothing painted"));
        assert!(near(g, p, tol), "{name}: geometry {g:?} vs painted {p:?}");
    }
    // The line: x extent exact; its y lies on the edge of the 5 px painted band (Figma strokes
    // the line on one side of its centerline).
    let g = local(layer_bbox(&m.proposal, "line-outline"));
    let p = painted(&png, grow(g)).expect("line painted");
    assert!(
        (g[0] - p[0]).abs() <= 1.0 && (g[2] - p[2]).abs() <= 1.0,
        "{g:?} vs {p:?}"
    );
    assert!(g[1] >= p[1] - 1.0 && g[1] <= p[3] + 1.0, "{g:?} vs {p:?}");
    // Union of the two boolean operands (child transforms compose with the group's).
    let (a, b) = (
        local(layer_bbox(&m.proposal, "union-rectangle")),
        local(layer_bbox(&m.proposal, "union-ellipse")),
    );
    let u = [
        a[0].min(b[0]),
        a[1].min(b[1]),
        a[2].max(b[2]),
        a[3].max(b[3]),
    ];
    let p = painted(&png, grow(u)).expect("union painted");
    assert!(near(u, p, 2.0), "union {u:?} vs painted {p:?}");
    // The boolean group itself has no Folio geometry home: result outline is a typed loss.
    let c = loss_counts(&m.proposal);
    assert_eq!(
        c[&(
            "native.boolean_operation".to_owned(),
            "boolean_operation_not_mapped"
        )],
        1
    );
}

#[test]
fn rotation_and_line_baked_and_network_fallback() {
    let m = mapped("fig-basic-shapes.fig");
    let cancel = CancellationToken::default();
    let cfg = NativeConfig {
        budget: Budget::default(),
        resolver: &Host,
    };
    // A 90 degree rotation about the node origin swaps the extents of the ellipse.
    let mut doc = m.doc.clone();
    let i = doc
        .nodes
        .iter()
        .position(|n| n.name.as_deref() == Some("ellipse-gradient"))
        .unwrap();
    doc.nodes[i].transform = Some(Matrix {
        m00: 0.0,
        m01: -1.0,
        m02: 250.0,
        m10: 1.0,
        m11: 0.0,
        m12: 90.0,
    });
    let p = to_native(&doc, &cfg, &mut SequentialIdMint::new(1), &cancel).unwrap();
    let b = layer_bbox(&p, "ellipse-gradient");
    // Local box 150 x 110 at (250,90) rotated: x in [250-110, 250], y in [90, 90+150], plus the
    // artboard offset (100,100).
    assert!(near(b, [240.0, 190.0, 350.0, 340.0], 1e-6), "{b:?}");
    // The pen vector without a decoded fill geometry falls back to its vector network.
    let mut doc = m.doc.clone();
    let i = doc
        .nodes
        .iter()
        .position(|n| n.name.as_deref() == Some("pen-freeform-bezier"))
        .unwrap();
    doc.nodes[i].fill_geometry.clear();
    let p = to_native(&doc, &cfg, &mut SequentialIdMint::new(1), &cancel).unwrap();
    let b = layer_bbox(&p, "pen-freeform-bezier");
    // Same node box as the blob geometry (handles included in both bboxes) within 12 px.
    let blob = layer_bbox(&m.proposal, "pen-freeform-bezier");
    assert!(near(b, blob, 12.0), "network {b:?} vs blob {blob:?}");
    assert!(
        b[0] >= 130.0 && b[2] <= 310.0 && b[1] >= 350.0 && b[3] <= 480.0,
        "{b:?}"
    );
    // The line has no geometry blob at all: segment from its size.
    let line = layer_bbox(&m.proposal, "line-outline");
    assert!(near(line, [550.0, 245.0, 700.0, 245.0], 1e-6), "{line:?}");
}

fn lum(png: &(usize, usize, Vec<u8>), x: usize, y: usize) -> i32 {
    let o = (y * png.0 + x) * 4;
    (0..3).map(|c| i32::from(png.2[o + c])).sum()
}

#[test]
fn real_effects_masks_image_paints_pinned() {
    use hsk_studio_folio::{StudioEffectKind, StudioMaskMode};
    use hsk_studio_package::ImportRole;
    let m = mapped("fig-basic-shapes.fig");
    let p = &m.proposal;
    let layer_id = |name: &str| {
        p.snapshot
            .document()
            .layers
            .iter()
            .find(|l| l.name == name)
            .unwrap_or_else(|| panic!("layer {name}"))
            .layer_id
            .clone()
    };
    // Drop shadow of rounded-solid-shadow (offset 4,6, radius 10, black 25%): one effect stack.
    assert_eq!(p.raster_records.layers.len(), 1);
    let rec = &p.raster_records.layers[0];
    assert_eq!(rec.layer_id, layer_id("rounded-solid-shadow"));
    let stack = rec.effects.as_ref().unwrap();
    assert_eq!(stack.effects.len(), 1);
    let fx = &stack.effects[0];
    assert!((fx.opacity - 25.0).abs() < 1e-9 && fx.enabled && fx.blend_mode == 2);
    assert!((fx.angle - 123.690067526).abs() < 1e-6, "{}", fx.angle);
    let StudioEffectKind::DropShadow {
        distance,
        spread,
        size,
        ..
    } = &fx.params
    else {
        panic!("drop shadow")
    };
    assert!((distance - 52f64.sqrt()).abs() < 1e-9 && *spread == 0.0 && *size == 10.0);
    // The render agrees: the shadow falls below the rectangle (darker under the bottom edge than
    // above the top edge, both 3 px away; artboard-local rectangle x 40..210, y 90..200).
    let png = decode_png(&fixture("fig-basic-shapes.png"));
    assert!(lum(&png, 125, 203) + 6 < lum(&png, 125, 87));
    // Blur: no Folio effect kind, still a typed loss; spread/choke notes absent here.
    let c = loss_counts(p);
    assert_eq!(
        c[&(
            "native.effects.FOREGROUND_BLUR".to_owned(),
            "effect_kind_not_in_folio"
        )],
        1
    );
    assert!(
        !c.keys()
            .any(|(path, _)| path == "native.effects.DROP_SHADOW")
    );
    // Mask: mask-ellipse masks its later sibling masked-gradient; the source layer is hidden.
    let v = &p.vector_records;
    assert_eq!(v.masks.len(), 1);
    assert_eq!(v.masks[0].source_layer_id, layer_id("mask-ellipse"));
    assert_eq!(v.masks[0].mode, StudioMaskMode::Alpha);
    assert_eq!(v.attachments.len(), 1);
    assert_eq!(v.attachments[0].layer_id, layer_id("masked-gradient"));
    assert_eq!(
        v.attachments[0].mask_id.as_deref(),
        Some(v.masks[0].mask_id.as_str())
    );
    let source = p
        .snapshot
        .document()
        .layers
        .iter()
        .find(|l| l.layer_id == v.masks[0].source_layer_id)
        .unwrap();
    assert!(!source.visible);
    assert!(!c.keys().any(|(path, _)| path == "native.mask"));
    // Image paint: bound to masked-gradient, no appearance row, part carried byte-exact.
    assert_eq!(p.image_paints.len(), 1);
    let img = &p.image_paints[0];
    assert_eq!(img.node.node_id, layer_id("masked-gradient"));
    assert_eq!((img.what, img.ordinal, img.visible), ("fills", 0, true));
    assert_eq!(img.original_size, Some((480, 320)));
    let part = img.part.as_deref().expect("image part present");
    assert_eq!(part, format!("images/{}", img.hash));
    let bytes = fixture("fig-basic-shapes.fig");
    let cancel = CancellationToken::default();
    let FigOutcome::Imported(import) = import_fig(
        &bytes,
        &Limits::default(),
        &VersionPolicy::observed(),
        None,
        &cancel,
    )
    .unwrap() else {
        panic!("imports")
    };
    let entries = p.import_entries(&import, &bytes).unwrap();
    let receipt: serde_json::Value = serde_json::from_slice(
        &entries
            .iter()
            .find(|e| e.role == ImportRole::Receipt)
            .unwrap()
            .bytes,
    )
    .unwrap();
    let want = import.container.part(part).expect("container part");
    let r = &receipt["image_paints"][0];
    assert_eq!(r["part_bytes"], want.bytes.len());
    assert_eq!(r["part_sha256"], hex(&want.sha256));
    assert!(want.bytes.starts_with(b"\x89PNG") || want.bytes.starts_with(&[0xFF, 0xD8]));
    assert_eq!(receipt["effect_stacks"], 1);
    assert_eq!(receipt["masks"], 1);
}

#[test]
fn real_text_fonts_bound_as_styles() {
    // (fixture, style values, text losses): Inter Regular/Bold with solid fills, one paragraph
    // style (100% line height, 0% letter spacing, top, width-and-height) per fixture.
    for (name, styles) in [
        ("fig-basic-shapes.fig", 3usize),
        ("fig-components-autolayout.fig", 3),
        ("fig-prototype-pages.fig", 3),
    ] {
        let m = mapped(name);
        let p = &m.proposal;
        assert_eq!(p.text_styles.len(), styles, "{name}");
        let c = loss_counts(p);
        assert!(
            !c.keys().any(|(path, _)| path.starts_with("native.text")),
            "{name}: no text losses"
        );
        // Every STYS binding of a story names a carried style of the right kind.
        for s in &p.stories {
            for r in &s.character_ranges {
                let st = p
                    .text_styles
                    .iter()
                    .find(|t| Some(&t.style_ref) == r.style_ref.as_ref())
                    .expect("character style carried");
                assert_eq!(st.kind, "character");
                assert_eq!(st.font_family.as_deref(), Some("Inter"));
                assert!(st.font_size.is_some() && st.fill.is_some());
            }
            for para in &s.paragraphs {
                let st = p
                    .text_styles
                    .iter()
                    .find(|t| Some(&t.style_ref) == para.style_ref.as_ref())
                    .expect("paragraph style carried");
                assert_eq!(st.kind, "paragraph");
                assert_eq!(st.line_height, Some((100.0, "PERCENT".to_owned())));
            }
        }
        // Folio holds no style record: the host resolver (absent here) is told every binding.
        assert!(!p.unresolved_text_styles.is_empty(), "{name}");
        assert!(
            p.unresolved_text_styles
                .iter()
                .all(|t| t.contains("/style/STYS-"))
        );
    }
    // mixed-style-text: "Regular text + " (15 bytes) Inter Regular 26, "bold text" Inter Bold 26.
    let m = mapped("fig-basic-shapes.fig");
    let p = &m.proposal;
    let layer = p
        .snapshot
        .document()
        .layers
        .iter()
        .find(|l| l.name == "mixed-style-text")
        .unwrap();
    let Payload::Text { story_id } = &layer.payload else {
        panic!("text layer")
    };
    let story = p.stories.iter().find(|s| &s.story_id == story_id).unwrap();
    let style_of = |i: usize| {
        let r = &story.character_ranges[i];
        let st = p
            .text_styles
            .iter()
            .find(|t| Some(&t.style_ref) == r.style_ref.as_ref())
            .unwrap();
        (r.start, r.end, st.font_style.clone().unwrap(), st.font_size)
    };
    assert_eq!(story.character_ranges.len(), 2);
    assert_eq!(style_of(0), (0, 15, "Regular".to_owned(), Some(26.0)));
    assert_eq!(style_of(1), (15, 24, "Bold".to_owned(), Some(26.0)));
}
