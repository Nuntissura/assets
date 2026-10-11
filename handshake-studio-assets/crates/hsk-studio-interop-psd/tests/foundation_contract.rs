//! Focused contract tests. Every input is assembled byte by byte below from the public file
//! format layout, independently of the crate's own encoder, so a symmetric bug cannot hide.

use hsk_studio_accord::CancellationToken;
use hsk_studio_interop_psd::*;

fn bounded_case(name: &'static str, body: impl FnOnce() + Send + 'static) {
    use std::{
        panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
        sync::mpsc,
        time::Duration,
    };
    let (sender, receiver) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let outcome = catch_unwind(AssertUnwindSafe(body));
        let _ = sender.send(outcome);
    });
    match receiver.recv_timeout(Duration::from_secs(30)) {
        Ok(Ok(())) => {}
        Ok(Err(p)) => resume_unwind(p),
        Err(mpsc::RecvTimeoutError::Timeout) => panic!("case {name} exceeded declared 30-second deadline"),
        Err(mpsc::RecvTimeoutError::Disconnected) => panic!("case {name} lost deadline worker result"),
    }
}

// ---- independent byte assembly -------------------------------------------------------------

fn be16(v: u16) -> Vec<u8> {
    v.to_be_bytes().to_vec()
}
fn be32(v: u32) -> Vec<u8> {
    v.to_be_bytes().to_vec()
}
fn be64(v: u64) -> Vec<u8> {
    v.to_be_bytes().to_vec()
}
fn len_field(psb_wide: bool, n: usize) -> Vec<u8> {
    if psb_wide { be64(n as u64) } else { be32(n as u32) }
}
fn pad_to(mut v: Vec<u8>, multiple: usize) -> Vec<u8> {
    while !v.len().is_multiple_of(multiple) {
        v.push(0);
    }
    v
}
fn unicode(s: &str) -> Vec<u8> {
    let units: Vec<u16> = s.encode_utf16().collect();
    let mut v = be32(units.len() as u32);
    for u in units {
        v.extend(be16(u));
    }
    v
}
fn packbits(row: &[u8]) -> Vec<u8> {
    if !row.is_empty() && row.iter().all(|&b| b == row[0]) && row.len() <= 128 && row.len() >= 2 {
        return vec![(257 - row.len()) as u8, row[0]];
    }
    let mut v = Vec::new();
    for chunk in row.chunks(128) {
        v.push((chunk.len() - 1) as u8);
        v.extend_from_slice(chunk);
    }
    v
}
/// RLE payload (counts then rows) for rows of one or more planes, PSD (2-byte) or PSB (4-byte) counts.
fn rle(rows: &[&[u8]], psb: bool) -> Vec<u8> {
    let packed: Vec<Vec<u8>> = rows.iter().map(|r| packbits(r)).collect();
    let mut v = Vec::new();
    for p in &packed {
        v.extend(if psb { be32(p.len() as u32) } else { be16(p.len() as u16) });
    }
    for p in &packed {
        v.extend_from_slice(p);
    }
    v
}
fn zlib(data: &[u8]) -> Vec<u8> {
    miniz_oxide::deflate::compress_to_vec_zlib(data, 6)
}
fn resource(id: u16, name: &str, data: &[u8]) -> Vec<u8> {
    let mut v = b"8BIM".to_vec();
    v.extend(be16(id));
    let mut pascal = vec![name.len() as u8];
    pascal.extend_from_slice(name.as_bytes());
    v.extend(pad_to(pascal, 2));
    v.extend(be32(data.len() as u32));
    v.extend_from_slice(data);
    if data.len() % 2 == 1 {
        v.push(0);
    }
    v
}
/// Layer-level block: the length field already includes alignment to 4.
fn layer_block(key: &[u8; 4], data: &[u8], psb: bool) -> Vec<u8> {
    let data = pad_to(data.to_vec(), 4);
    let mut v = b"8BIM".to_vec();
    v.extend_from_slice(key);
    v.extend(len_field(psb && matches!(key, b"Lr16" | b"Layr" | b"LMsk"), data.len()));
    v.extend(data);
    v
}
/// File-level block: the length field excludes the alignment to 4.
fn global_block(key: &[u8; 4], data: &[u8], psb: bool) -> Vec<u8> {
    let mut v = b"8BIM".to_vec();
    v.extend_from_slice(key);
    v.extend(len_field(psb && matches!(key, b"Lr16" | b"Layr" | b"LMsk"), data.len()));
    v.extend_from_slice(data);
    while !v.len().is_multiple_of(4) {
        v.push(0);
    }
    v
}

#[derive(Clone)]
struct TLayer {
    name: &'static str,
    rect: [i32; 4],
    channels: Vec<(i16, u16, Vec<u8>)>,
    key: [u8; 4],
    opacity: u8,
    clipping: u8,
    flags: u8,
    mask: Vec<u8>,
    ranges: Vec<u8>,
    blocks: Vec<Vec<u8>>,
}
impl TLayer {
    fn new(name: &'static str, rect: [i32; 4]) -> Self {
        Self {
            name,
            rect,
            channels: Vec::new(),
            key: *b"norm",
            opacity: 255,
            clipping: 0,
            flags: 0x08,
            mask: Vec::new(),
            ranges: Vec::new(),
            blocks: Vec::new(),
        }
    }
    fn record(&self, psb: bool) -> Vec<u8> {
        let mut v = Vec::new();
        for r in self.rect {
            v.extend(r.to_be_bytes());
        }
        v.extend(be16(self.channels.len() as u16));
        for (id, _, payload) in &self.channels {
            v.extend(id.to_be_bytes());
            v.extend(len_field(psb, 2 + payload.len()));
        }
        v.extend_from_slice(b"8BIM");
        v.extend_from_slice(&self.key);
        v.extend([self.opacity, self.clipping, self.flags, 0]);
        let mut extra = be32(self.mask.len() as u32);
        extra.extend_from_slice(&self.mask);
        extra.extend(be32(self.ranges.len() as u32));
        extra.extend_from_slice(&self.ranges);
        let mut pascal = vec![self.name.len() as u8];
        pascal.extend_from_slice(self.name.as_bytes());
        extra.extend(pad_to(pascal, 4));
        for b in &self.blocks {
            extra.extend_from_slice(b);
        }
        v.extend(be32(extra.len() as u32));
        v.extend(extra);
        v
    }
    fn channel_data(&self) -> Vec<u8> {
        let mut v = Vec::new();
        for (_, comp, payload) in &self.channels {
            v.extend(be16(*comp));
            v.extend_from_slice(payload);
        }
        v
    }
}
fn layer_info(layers: &[TLayer], psb: bool, negative_count: bool) -> Vec<u8> {
    let n = layers.len() as i16;
    let mut v = (if negative_count { -n } else { n }).to_be_bytes().to_vec();
    for l in layers {
        v.extend(l.record(psb));
    }
    for l in layers {
        v.extend(l.channel_data());
    }
    pad_to(v, 2)
}
/// Layer and mask information section: layer info, empty global mask, global blocks.
fn layer_section(info: &[u8], blocks: &[Vec<u8>], psb: bool) -> Vec<u8> {
    let mut v = len_field(psb, info.len());
    v.extend_from_slice(info);
    v.extend(be32(0));
    for b in blocks {
        v.extend_from_slice(b);
    }
    v
}
struct Doc {
    version: u16,
    channels: u16,
    height: u32,
    width: u32,
    depth: u16,
    mode: u16,
    color_mode_data: Vec<u8>,
    resources: Vec<u8>,
    layer_section: Vec<u8>,
    image: Option<(u16, Vec<u8>)>,
}
impl Doc {
    fn bytes(&self) -> Vec<u8> {
        let mut v = b"8BPS".to_vec();
        v.extend(be16(self.version));
        v.extend([0; 6]);
        v.extend(be16(self.channels));
        v.extend(be32(self.height));
        v.extend(be32(self.width));
        v.extend(be16(self.depth));
        v.extend(be16(self.mode));
        v.extend(be32(self.color_mode_data.len() as u32));
        v.extend_from_slice(&self.color_mode_data);
        v.extend(be32(self.resources.len() as u32));
        v.extend_from_slice(&self.resources);
        v.extend(len_field(self.version == 2, self.layer_section.len()));
        v.extend_from_slice(&self.layer_section);
        if let Some((comp, data)) = &self.image {
            v.extend(be16(*comp));
            v.extend_from_slice(data);
        }
        v
    }
}

fn empty_channels(ids: &[i16]) -> Vec<(i16, u16, Vec<u8>)> {
    ids.iter().map(|&id| (id, 0, Vec::new())).collect()
}

/// The main fixture: 4x2 RGB 8-bit, six layers in file (bottom to top) order:
/// 0 base, 1 group divider, 2 bg, 3 fg, 4 zip, 5 group folder; RLE merged image.
fn fixture() -> Vec<u8> {
    let mut base = TLayer::new("base", [0, 0, 1, 1]);
    base.channels = vec![(0, 0, vec![200]), (1, 0, vec![100]), (2, 0, vec![50])];

    let mut divider = TLayer::new("</Layer group>", [0, 0, 0, 0]);
    divider.channels = empty_channels(&[0, 1, 2]);
    divider.blocks = vec![layer_block(b"lsct", &be32(3), false)];

    let mut bg = TLayer::new("bg", [0, 0, 2, 4]);
    bg.key = *b"mul ";
    bg.opacity = 128;
    bg.channels = vec![
        (0, 0, vec![1, 2, 3, 4, 5, 6, 7, 8]),
        (1, 0, vec![11, 12, 13, 14, 15, 16, 17, 18]),
        (2, 0, vec![21, 22, 23, 24, 25, 26, 27, 28]),
        (-1, 0, vec![255, 255, 0, 0, 255, 255, 0, 0]),
    ];

    let mut fg = TLayer::new("fg", [0, 1, 2, 3]);
    fg.key = *b"scrn";
    fg.clipping = 1;
    fg.flags = 0x0A;
    fg.channels = vec![
        (0, 1, rle(&[&[9, 9], &[1, 2]], false)),
        (1, 1, rle(&[&[8, 8], &[3, 4]], false)),
        (2, 1, rle(&[&[7, 7], &[5, 6]], false)),
    ];

    let mut zip = TLayer::new("zip", [0, 0, 1, 3]);
    zip.channels = vec![
        (0, 2, zlib(&[100, 101, 102])),
        (1, 3, zlib(&[10, 10, 10])),
        (2, 0, vec![0, 0, 0]),
    ];

    let mut folder = TLayer::new("Group u", [0, 0, 0, 0]);
    folder.key = *b"pass";
    folder.channels = empty_channels(&[0, 1, 2]);
    let mut lsct = be32(1);
    lsct.extend_from_slice(b"8BIM");
    lsct.extend_from_slice(b"pass");
    folder.blocks = vec![layer_block(b"lsct", &lsct, false), layer_block(b"luni", &unicode("Group \u{fc}"), false)];

    let info = layer_info(&[base, divider, bg, fg, zip, folder], false, false);

    let mut version_info = be32(1);
    version_info.push(1);
    version_info.extend(unicode("Adobe Photoshop"));
    version_info.extend(unicode("Adobe Photoshop"));
    version_info.extend(be32(1));
    let mut resources = resource(1005, "abc", &[1, 2, 3]);
    resources.extend(resource(1039, "", &[9, 8, 7, 6, 5]));
    resources.extend(resource(1057, "", &version_info));

    let reds: [u8; 8] = [10, 20, 30, 40, 50, 60, 70, 80];
    let greens: [u8; 8] = [1, 1, 1, 1, 2, 2, 2, 2];
    let blues: [u8; 8] = [0; 8];
    let rows: Vec<&[u8]> = vec![
        &reds[..4], &reds[4..], &greens[..4], &greens[4..], &blues[..4], &blues[4..],
    ];

    Doc {
        version: 1,
        channels: 3,
        height: 2,
        width: 4,
        depth: 8,
        mode: 3,
        color_mode_data: Vec::new(),
        resources,
        layer_section: layer_section(&info, &[global_block(b"XYZW", &[1, 2, 3, 4, 5], false)], false),
        image: Some((1, rle(&rows, false))),
    }
    .bytes()
}

/// PSB, 16-bit, 2x1 RGB; the layer list lives in an `Lr16` global block with an 8-byte length.
fn psb_fixture() -> Vec<u8> {
    let mut layer = TLayer::new("deep", [0, 0, 1, 2]);
    layer.channels = vec![(0, 1, rle(&[&[0, 1, 0, 2]], true)), (1, 0, vec![0, 3, 0, 4]), (2, 2, zlib(&[0, 5, 0, 6]))];
    let info = layer_info(&[layer], true, false);
    Doc {
        version: 2,
        channels: 3,
        height: 1,
        width: 2,
        depth: 16,
        mode: 3,
        color_mode_data: Vec::new(),
        resources: Vec::new(),
        layer_section: layer_section(&[], &[global_block(b"Lr16", &info, true)], true),
        image: Some((0, vec![0, 1, 0, 2, 0, 3, 0, 4, 0, 5, 0, 6])),
    }
    .bytes()
}

fn read(bytes: &[u8]) -> PsdDocument {
    read_psd(bytes, &Limits::default(), &CancellationToken::default()).expect("fixture parses")
}
fn code_of(bytes: &[u8], limits: &Limits) -> &'static str {
    read_psd(bytes, limits, &CancellationToken::default()).expect_err("must be rejected").code()
}

// ---- tests ---------------------------------------------------------------------------------

#[test]
fn layers_groups_resources_and_merged_decode() {
    bounded_case("layers_groups_resources_and_merged_decode", || {
        let limits = Limits::default();
        let cancel = CancellationToken::default();
        let doc = read(&fixture());
        assert_eq!(doc.header.version, 1);
        assert_eq!((doc.header.width, doc.header.height, doc.header.depth), (4, 2, 8));
        assert_eq!(doc.header.color_mode, ColorMode::Rgb);

        // resources: ICC bytes exact (odd length padded), version info, unknown resource kept
        assert_eq!(doc.resources.icc_profile(), Some(&[9u8, 8, 7, 6, 5][..]));
        let info = doc.resources.version_info(1024).expect("1057");
        assert!(info.has_real_merged_data);
        assert_eq!(info.writer_name.as_deref(), Some("Adobe Photoshop"));
        assert_eq!(doc.merged_status(&limits), MergedStatus::Real);
        assert_eq!(doc.resources.find(1005).expect("1005").name, b"abc");
        assert!(doc.resources.trailing.is_empty());

        // layer records
        let layers = doc.layers();
        assert_eq!(layers.len(), 6);
        let names: Vec<&str> = layers.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(names, ["base", "</Layer group>", "bg", "fg", "zip", "Group \u{fc}"]);
        let bg = &layers[2];
        assert_eq!((bg.blend_mode(), bg.opacity, bg.clipping), (Some(BlendMode::Multiply), 128, 0));
        assert!(bg.visible());
        let fg = &layers[3];
        assert_eq!((fg.blend_mode(), fg.clipping), (Some(BlendMode::Screen), 1));
        assert!(fg.hidden(), "flags bit 1 set means hidden");
        assert_eq!(layers[5].blend_mode(), Some(BlendMode::PassThrough));
        assert_eq!(layers[5].group_role(), GroupRole::OpenFolder);
        assert_eq!(layers[5].group_blend_key(), Some(*b"pass"));
        assert_eq!(layers[1].group_role(), GroupRole::BoundingDivider);

        // hierarchy (panel order, top-most first)
        assert_eq!(
            doc.tree(&limits).expect("tree"),
            vec![
                TreeNode::Group {
                    folder: 5,
                    divider: 1,
                    children: vec![TreeNode::Layer { index: 4 }, TreeNode::Layer { index: 3 }, TreeNode::Layer { index: 2 }],
                },
                TreeNode::Layer { index: 0 },
            ]
        );

        // channel decode across all four compressions, odd widths, alpha
        let plane = |l: usize, id: i16| {
            let p = layers[l].decode_channel(id, &doc.header, &limits, &cancel).expect("decode").expect("channel");
            p.bytes
        };
        assert_eq!(plane(0, 0), [200]);
        assert_eq!(plane(2, 1), [11, 12, 13, 14, 15, 16, 17, 18]);
        assert_eq!(plane(2, -1), [255, 255, 0, 0, 255, 255, 0, 0]);
        assert_eq!(plane(3, 0), [9, 9, 1, 2]);
        assert_eq!(plane(3, 2), [7, 7, 5, 6]);
        assert_eq!(plane(4, 0), [100, 101, 102], "zip");
        assert_eq!(plane(4, 1), [10, 20, 30], "zip with prediction");
        assert_eq!(plane(4, 2), [0, 0, 0]);
        assert!(layers[4].decode_channel(-2, &doc.header, &limits, &cancel).expect("decode").is_none());

        // merged composite decodes to the exact assembled pixels
        let merged = doc.decode_merged(&limits, &cancel).expect("merged").expect("present");
        let reds = [10u8, 20, 30, 40, 50, 60, 70, 80];
        let greens = [1u8, 1, 1, 1, 2, 2, 2, 2];
        let expected: Vec<u8> = (0..8).flat_map(|i| [reds[i], greens[i], 0, 255]).collect();
        assert_eq!(merged.to_rgba8().expect("rgba"), expected);

        // global block with length excluding its 4-byte alignment
        assert_eq!(doc.layer_section.blocks.len(), 1);
        assert_eq!(doc.layer_section.blocks[0].data, [1, 2, 3, 4, 5]);
        assert_eq!(doc.layer_section.blocks[0].pad.len(), 3);
    });
}

#[test]
fn compression_matrix_and_psb_wide_fields() {
    bounded_case("compression_matrix_and_psb_wide_fields", || {
        let limits = Limits::default();
        let cancel = CancellationToken::default();
        let geo = |width, height, depth, psb| PlaneGeometry { width, height, depth, planes: 1, psb };
        let dec = |c, data: &[u8], g| decode_samples(c, data, g, &limits, &cancel);

        // raw, RLE with a no-op header (0x80) and a run, ZIP, ZIP+prediction (8-bit)
        assert_eq!(dec(COMPRESSION_RAW, &[1, 2, 3, 4, 5, 6], geo(3, 2, 8, false)).unwrap(), [1, 2, 3, 4, 5, 6]);
        // 0xFE repeats the next byte 257 - 254 = 3 times
        let rows = [vec![0xFE, 1], vec![0x80, 0x02, 1, 2, 3]];
        let mut rle_data = vec![0, 2, 0, 5];
        rle_data.extend(&rows[0]);
        rle_data.extend(&rows[1]);
        assert_eq!(dec(COMPRESSION_RLE, &rle_data, geo(3, 2, 8, false)).unwrap(), [1, 1, 1, 1, 2, 3]);
        assert_eq!(dec(COMPRESSION_ZIP, &zlib(&[9, 8, 7, 6, 5, 4]), geo(3, 2, 8, false)).unwrap(), [9, 8, 7, 6, 5, 4]);
        assert_eq!(
            dec(COMPRESSION_ZIP_PREDICTION, &zlib(&[5, 2, 2, 1, 1, 1]), geo(3, 2, 8, false)).unwrap(),
            [5, 7, 9, 1, 2, 3]
        );
        // 16-bit: raw bytes, and word-wise prediction (behaviour reference, depth 16 UNVERIFIED vs Photoshop)
        let words = [0x0102u16, 0x0101, 0x0101];
        let delta: Vec<u8> = words.iter().flat_map(|w| w.to_be_bytes()).collect();
        assert_eq!(
            dec(COMPRESSION_ZIP_PREDICTION, &zlib(&delta), geo(3, 1, 16, false)).unwrap(),
            [0x01, 0x02, 0x02, 0x03, 0x03, 0x04]
        );
        // PSB RLE uses 4-byte row counts
        let mut psb_rle = be32(2);
        psb_rle.extend(be32(2));
        psb_rle.extend([0xFE, 7, 0xFE, 8]);
        assert_eq!(dec(COMPRESSION_RLE, &psb_rle, geo(3, 2, 8, true)).unwrap(), [7, 7, 7, 8, 8, 8]);

        // PSB document, 16-bit: layer list stored in an `Lr16` block with an 8-byte length
        let parsed = read(&psb_fixture());
        assert!(parsed.header.is_psb());
        let info = parsed.layer_section.layer_info.as_ref().expect("layer info from Lr16");
        assert!(matches!(info.location, LayerInfoLocation::Tagged { key: [b'L', b'r', b'1', b'6'], .. }));
        assert!(parsed.layer_section.blocks.is_empty());
        let l = &parsed.layers()[0];
        let p = |id| l.decode_channel(id, &parsed.header, &limits, &cancel).unwrap().unwrap();
        assert_eq!(p(0).bytes, [0, 1, 0, 2]);
        assert_eq!(p(0).sample(1, 0), Some(2));
        assert_eq!(p(1).bytes, [0, 3, 0, 4]);
        assert_eq!(p(2).bytes, [0, 5, 0, 6]);
        let merged = parsed.decode_merged(&limits, &cancel).unwrap().unwrap();
        assert_eq!(merged.planes.len(), 3);
        assert_eq!(merged.planes[2].bytes, [0, 5, 0, 6]);
        assert_eq!(parsed.header.depth, 16);
    });
}

#[test]
fn unknown_blend_and_unmapped_fields_are_typed_losses() {
    bounded_case("unknown_blend_and_unmapped_fields_are_typed_losses", || {
        let limits = Limits::default();
        let mut l = TLayer::new("odd", [0, 0, 1, 1]);
        l.key = *b"xxxx";
        l.channels = vec![(0, 0, vec![1]), (1, 0, vec![2]), (2, 0, vec![3])];
        l.mask = {
            let mut m = Vec::new();
            for v in [0i32, 0, 1, 1] {
                m.extend(v.to_be_bytes());
            }
            m.extend([255, 0, 0, 0]);
            m
        };
        // non-default blend-if: composite source narrowed
        l.ranges = vec![10, 20, 200, 210, 0, 0, 255, 255];
        l.blocks = vec![
            layer_block(b"TySh", &[0, 1, 2, 3], false),
            layer_block(b"lfx2", &[0; 8], false),
            layer_block(b"ABCD", &[7, 7], false),
            layer_block(b"levl", &[0; 4], false),
            layer_block(b"clbl", &[1], false),
        ];
        let info = layer_info(&[l], false, false);
        let bytes = Doc {
            version: 1,
            channels: 3,
            height: 1,
            width: 1,
            depth: 8,
            mode: 3,
            color_mode_data: Vec::new(),
            resources: resource(1005, "", &[1]),
            layer_section: layer_section(&info, &[], false),
            image: Some((0, vec![1, 2, 3])),
        }
        .bytes();
        let doc = read(&bytes);
        let layer = &doc.layers()[0];
        assert_eq!(layer.blend_mode(), None, "unknown key is not mapped to Normal");
        assert_eq!(layer.blend_key, *b"xxxx");
        assert_eq!(layer.blend_clipped_elements(), Some(true));
        assert_eq!(doc.unknown_blend_keys(), vec![*b"xxxx"]);
        let report = doc.loss_report(&limits);
        let code = |path: &str| report.find(path).map(|e| (e.kind, e.code));
        assert_eq!(code("layer[0]/blend"), Some((LossKind::Unsupported, "blend_mode_unknown")));
        assert_eq!(code("layer[0]/tagged/TySh"), Some((LossKind::PreservedOpaque, "text_layer_not_editable")));
        assert_eq!(code("layer[0]/tagged/lfx2"), Some((LossKind::PreservedOpaque, "layer_effects_not_rendered")));
        assert_eq!(code("layer[0]/tagged/ABCD"), Some((LossKind::PreservedOpaque, "tagged_block_opaque")));
        assert_eq!(code("layer[0]/tagged/levl"), Some((LossKind::PreservedOpaque, "adjustment_layer_not_applied")));
        assert_eq!(code("layer[0]/mask"), Some((LossKind::PreservedOpaque, "layer_mask_not_applied")));
        assert_eq!(code("layer[0]/blend_ranges"), Some((LossKind::PreservedOpaque, "blend_if_not_applied")));
        assert_eq!(code("resource[1005]"), Some((LossKind::PreservedOpaque, "image_resource_opaque")));
        assert_eq!(code("merged_image"), Some((LossKind::Preserved, "merged_image_oracle_unknown")));
        assert!(report.find("layer[0]/tagged/clbl").is_none(), "clbl is mapped, not lost");
        assert!(report.to_json().starts_with("{\"entries\":[{\"path\":\"resource[1005]\""));
        // the mask and the unknown block survive verbatim in the model
        assert_eq!(layer.mask().map(|m| (m.rect.right, m.default_color)), Some((1, 255)));
        assert_eq!(layer.block(b"ABCD").map(|b| b.data.clone()), Some(vec![7, 7, 0, 0]));
    });
}

#[test]
fn hostile_inputs_are_rejected_with_distinct_codes() {
    bounded_case("hostile_inputs_are_rejected_with_distinct_codes", || {
        let good = fixture();
        let limits = Limits::default();
        let patch = |at: usize, with: &[u8]| {
            let mut v = good.clone();
            v[at..at + with.len()].copy_from_slice(with);
            v
        };
        let mut codes = std::collections::BTreeSet::new();
        let mut check = |bytes: &[u8], limits: &Limits, expected: &'static str| {
            let got = code_of(bytes, limits);
            assert_eq!(got, expected);
            codes.insert(got);
        };
        check(&patch(0, b"8BPX"), &limits, "psd_bad_signature");
        check(&patch(4, &[0, 3]), &limits, "psd_unsupported_version");
        check(&patch(6, &[1]), &limits, "psd_bad_reserved");
        check(&patch(12, &[0, 0]), &limits, "psd_bad_channel_count");
        check(&patch(14, &[0, 0, 0, 0]), &limits, "psd_bad_dimensions");
        check(&patch(22, &[0, 7]), &limits, "psd_bad_depth");
        check(&patch(24, &[0, 5]), &limits, "psd_bad_color_mode");
        check(&good[..10], &limits, "psd_truncated");
        check(&good, &Limits { max_pixels: 4, ..Limits::default() }, "psd_pixel_limit");
        check(&good, &Limits { max_layers: 2, ..Limits::default() }, "psd_layer_limit");
        check(&good, &Limits { max_input_bytes: 16, ..Limits::default() }, "psd_input_too_large");
        // layer and mask section length past the end of the file
        let section_len_at = 26 + 4 + 4 + u32::from_be_bytes([good[30], good[31], good[32], good[33]]) as usize;
        check(&patch(section_len_at, &[0xFF, 0xFF, 0xFF, 0xFF]), &limits, "psd_truncated");

        // codec failures
        let cancel = CancellationToken::default();
        let geo = PlaneGeometry { width: 4, height: 1, depth: 8, planes: 1, psb: false };
        let dec = |c, data: &[u8]| decode_samples(c, data, geo, &limits, &cancel).unwrap_err().code();
        assert_eq!(dec(COMPRESSION_RLE, &[0, 2, 0xFE, 1]), "psd_rle_short");
        assert_eq!(dec(COMPRESSION_RLE, &[0, 2, 0xF0, 1]), "psd_rle_overflow");
        assert_eq!(dec(COMPRESSION_RLE, &[0]), "psd_rle_row_counts");
        assert_eq!(dec(COMPRESSION_RLE, &[0, 9, 0, 0]), "psd_truncated");
        assert_eq!(dec(COMPRESSION_RAW, &[1, 2]), "psd_channel_size");
        assert_eq!(dec(COMPRESSION_ZIP, &[1, 2, 3]), "psd_zip_failed");
        assert_eq!(dec(COMPRESSION_ZIP, &zlib(&[0u8; 5000])), "psd_zip_failed", "inflation beyond the declared plane is refused");
        assert_eq!(dec(COMPRESSION_ZIP, &zlib(&[1, 2])), "psd_channel_size");
        assert_eq!(dec(9, &[]), "psd_bad_compression");

        // every truncated prefix and a corrupted body must return, never panic
        for end in 0..good.len() {
            let _ = read_psd(&good[..end], &limits, &cancel);
        }
        for at in 26..good.len() {
            let mut v = good.clone();
            v[at] ^= 0xFF;
            let _ = read_psd(&v, &limits, &cancel);
        }

        // cooperative cancellation
        let token = CancellationToken::default();
        token.cancel();
        assert_eq!(read_psd(&good, &limits, &token).unwrap_err().code(), "psd_canceled");
        assert!(codes.len() >= 11, "codes are distinct per cause: {codes:?}");
    });
}

#[test]
fn unmodified_roundtrip_is_byte_identical_and_rename_touches_only_the_name() {
    bounded_case("unmodified_roundtrip_is_byte_identical_and_rename_touches_only_the_name", || {
        let limits = Limits::default();
        for original in [fixture(), psb_fixture()] {
            let doc = read(&original);
            assert_eq!(write_psd(&doc, &limits).expect("write"), original, "unmodified re-emission is exact");
        }

        let original = fixture();
        let doc = read(&original);
        let mut edited = doc.clone();
        {
            let layers = &mut edited.layer_section.layer_info.as_mut().expect("layers").layers;
            layers[5].name = "Folder \u{4e2d}".to_owned(); // has luni: replaced
            layers[2].name = "caf\u{e9}\u{4e2d}".to_owned(); // no luni: added, Pascal falls back to Latin-1
        }
        let bytes = write_psd(&edited, &limits).expect("write edited");
        let back = read(&bytes);
        assert_eq!(back.layers()[5].name, "Folder \u{4e2d}");
        assert_eq!(back.layers()[2].name, "caf\u{e9}\u{4e2d}");
        assert_eq!(back.layers()[2].name_pascal, b"caf\xe9?");
        assert_eq!(back.layers()[5].group_role(), GroupRole::OpenFolder, "lsct untouched");

        // everything else is identical: other layers, channel payloads, resources, merged image, global blocks
        for i in [0usize, 1, 3, 4] {
            assert_eq!(back.layers()[i], doc.layers()[i]);
        }
        for i in [2usize, 5] {
            assert_eq!(back.layers()[i].channels, doc.layers()[i].channels);
            assert_eq!(back.layers()[i].rect, doc.layers()[i].rect);
            assert_eq!(back.layers()[i].blend_key, doc.layers()[i].blend_key);
            assert_eq!(back.layers()[i].opacity, doc.layers()[i].opacity);
        }
        assert_eq!(back.resources, doc.resources);
        assert_eq!(back.image_data, doc.image_data);
        assert_eq!(back.layer_section.blocks, doc.layer_section.blocks);
        // the edit is stable: re-reading and re-writing changes nothing further
        assert_eq!(write_psd(&back, &limits).expect("rewrite"), bytes);
    });
}
