//! Folio kinds added by SMB-FOLIO-PAINT-BLUR-STYLE, mapped from the three Operator Figma fixtures
//! (lane SMB-INTEROP-FIG-FOLIO-KINDS): blur effects, image paints bound to the import-original
//! part, boolean groups with their cached outline, and type style records resolving the STYS ids
//! of the stories. Includes the real package save and reopen of all of them.
use hsk_studio_accord::{CancellationToken, SCHEMA_STUDIO_COMPONENT_INSTANCE};
use hsk_studio_folio::{
    BlurScope, Budget, ContentDigest, Payload, Resolution, Resolver, StudioBooleanOperation,
    StudioEffectKind, StudioImageScale, StudioImageSource, StudioLetterSpacing, StudioLineHeight,
    TileRef, TypeStyleResolver, TypeStyleScope, check_story_style_scopes, validate_text_story,
    validate_type_style_records, validate_vector_records,
};
use hsk_studio_interop_fig::*;
use hsk_studio_package::{
    Extras, ImportRole, Limits as PackageLimits, MemoryAssetSource, WriteOptions, read_package,
    write_package_with,
};
use sha2::{Digest, Sha256};
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

const FIXTURES: [&str; 3] = [
    "fig-basic-shapes.fig",
    "fig-components-autolayout.fig",
    "fig-prototype-pages.fig",
];

fn fixture(name: &str) -> Vec<u8> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("fixture {name}: {e}"))
}

struct Mapped {
    bytes: Vec<u8>,
    import: FigImport,
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
    Mapped {
        bytes,
        import: *import,
        doc,
        proposal,
    }
}

fn losses(p: &NativeProposal) -> BTreeMap<(String, &'static str), u32> {
    p.mapping_loss
        .iter()
        .map(|l| ((l.path.clone(), l.code), l.count))
        .collect()
}

fn layer_id(p: &NativeProposal, name: &str) -> String {
    p.snapshot
        .document()
        .layers
        .iter()
        .find(|l| l.name == name)
        .unwrap_or_else(|| panic!("layer {name}"))
        .layer_id
        .clone()
}

fn sha(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[test]
fn real_blur_maps_to_folio_blur() {
    let m = mapped("fig-basic-shapes.fig");
    let p = &m.proposal;
    // layer-blur: FOREGROUND_BLUR -> Blur { layer, radius }, no colour or contour.
    let rec = p
        .raster_records
        .layer(&layer_id(p, "layer-blur"))
        .expect("blur record");
    let fx = &rec.effects.as_ref().unwrap().effects;
    assert_eq!(fx.len(), 1);
    assert!(fx[0].colour.is_none() && fx[0].contour.is_none() && fx[0].enabled);
    let StudioEffectKind::Blur {
        render_scope,
        radius,
    } = &fx[0].params
    else {
        panic!("blur effect")
    };
    assert_eq!(*render_scope, BlurScope::Layer);
    let src = m
        .doc
        .named("layer-blur")
        .next()
        .unwrap()
        .effects
        .iter()
        .find(|e| e.kind == "FOREGROUND_BLUR")
        .unwrap()
        .radius
        .unwrap();
    assert_eq!(*radius, f64::from(src.to_string().parse::<f32>().unwrap()));
    assert!(*radius > 0.0);
    // The shadow layer keeps its shadow stack.
    assert!(
        p.raster_records
            .layer(&layer_id(p, "rounded-solid-shadow"))
            .is_some()
    );
    for gone in [
        "native.effects.FOREGROUND_BLUR",
        "native.effects.BACKGROUND_BLUR",
    ] {
        assert!(!losses(p).keys().any(|(path, _)| path == gone));
    }
}

#[test]
fn real_image_fill_maps_to_import_original_part() {
    let m = mapped("fig-basic-shapes.fig");
    let p = &m.proposal;
    let v = &p.vector_records;
    assert_eq!(v.image_paints.len(), 1);
    let img = &v.image_paints[0];
    // Bound to row 0 of the masked-gradient layer's paint style, which holds the stand-in row.
    let design = p
        .design
        .nodes
        .iter()
        .find(|d| d.node.node_id == layer_id(p, "masked-gradient"))
        .unwrap();
    assert_eq!(design.style_refs["fill_style_id"], img.style_id);
    assert_eq!(img.row, 0);
    assert!(v.image_paint(&img.style_id, 0).is_some());
    let StudioImageSource::ImportOriginal {
        entry_digest,
        part,
        content_digest,
    } = &img.image
    else {
        panic!("import-original source")
    };
    // entry digest = sha256 of the original .fig; content digest = sha256 of images/<hash>.
    assert_eq!(entry_digest.digest, sha(&m.bytes));
    let held = m.import.container.part(part).expect("part in container");
    assert!(part.starts_with("images/"));
    assert_eq!(content_digest.digest, sha(&held.bytes));
    assert_eq!(
        *content_digest,
        ContentDigest {
            algorithm: "sha256".to_owned(),
            digest: hex(&held.sha256),
        }
    );
    // Scale mode and transform: the fixture's decoded paint is the source of both.
    let fills = &m.doc.named("masked-gradient").next().unwrap().fills;
    let PaintKind::Image {
        scale_mode,
        transform,
        ..
    } = &fills[0].kind
    else {
        panic!("image paint")
    };
    let want = match scale_mode.as_deref() {
        None | Some("FILL") => StudioImageScale::Fill,
        Some("FIT") => StudioImageScale::Fit,
        Some("CROP") | Some("STRETCH") => StudioImageScale::Crop,
        Some("TILE") => StudioImageScale::Tile,
        other => panic!("scale mode {other:?}"),
    };
    assert_eq!(img.scale_mode, want);
    if transform.is_none() {
        assert_eq!(img.transform, [1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
    }
    assert_eq!(
        (img.scaling_factor, img.rotation, img.opacity),
        (1.0, 0.0, 100.0)
    );
    assert!(!losses(p).keys().any(|(path, _)| path.ends_with(".IMAGE")));
    // The receipt repeats the part identity.
    let entries = p.import_entries(&m.import, &m.bytes).unwrap();
    let receipt: serde_json::Value = serde_json::from_slice(
        &entries
            .iter()
            .find(|e| e.role == ImportRole::Receipt)
            .unwrap()
            .bytes,
    )
    .unwrap();
    assert_eq!(
        receipt["image_paints"][0]["part_sha256"],
        content_digest.digest
    );
    assert_eq!(
        receipt["image_paints"][0]["recorded_sha256"],
        content_digest.digest
    );
}

#[test]
fn real_boolean_operation_maps_to_group_with_cached_outline() {
    let m = mapped("fig-basic-shapes.fig");
    let p = &m.proposal;
    let id = layer_id(p, "boolean-union");
    let g = p.vector_records.boolean_group(&id).expect("boolean group");
    assert_eq!(g.operation, StudioBooleanOperation::Union);
    // Operands: the two union children, bottom first.
    assert_eq!(
        g.operands,
        [layer_id(p, "union-rectangle"), layer_id(p, "union-ellipse")]
    );
    assert!(!g.fill_geometry_path_ids.is_empty());
    for path in &g.fill_geometry_path_ids {
        let rec = p
            .paths
            .iter()
            .find(|x| &x.path_id == path)
            .expect("outline path");
        assert_eq!(rec.layer_id, id);
    }
    // The group layer stays a plain group; the loss is gone.
    let layer = p
        .snapshot
        .document()
        .layers
        .iter()
        .find(|l| l.layer_id == id)
        .unwrap();
    assert!(matches!(layer.payload, Payload::Group));
    assert!(
        !losses(p)
            .keys()
            .any(|(path, _)| path == "native.boolean_operation")
    );
}

#[test]
fn real_text_styles_resolve_stys_ids() {
    let m = mapped("fig-basic-shapes.fig");
    let p = &m.proposal;
    let set = &p.type_styles;
    // Two character styles (Inter Regular / Bold 26, solid fill) and one paragraph style.
    assert_eq!(set.styles.len(), 3);
    assert!(p.unresolved_text_styles.is_empty());
    let character: Vec<_> = set
        .styles
        .iter()
        .filter(|s| s.scope == TypeStyleScope::Character)
        .collect();
    assert_eq!(character.len(), 2);
    for s in &character {
        assert_eq!(s.font_family.as_deref(), Some("Inter"));
        assert_eq!(s.font_size.as_ref().map(|l| l.value), Some(26.0));
        assert!(s.fill.is_some());
    }
    let styles: Vec<_> = character
        .iter()
        .map(|s| s.font_style.clone().unwrap())
        .collect();
    assert_eq!(styles, ["Regular", "Bold"]);
    let para = set
        .styles
        .iter()
        .find(|s| s.scope == TypeStyleScope::Paragraph)
        .unwrap();
    assert_eq!(
        para.line_height,
        Some(StudioLineHeight::Percent { value: 100.0 })
    );
    assert_eq!(
        para.letter_spacing,
        Some(StudioLetterSpacing::Percent { value: 0.0 })
    );
    // Every story resolves through the set: no issue, scopes consistent.
    let resolver = TypeStyleResolver {
        styles: set,
        inner: &Host,
    };
    for s in &p.stories {
        let issues = validate_text_story(
            s,
            Budget::default(),
            &CancellationToken::default(),
            &resolver,
        )
        .unwrap();
        assert!(issues.is_empty(), "{issues:?}");
        check_story_style_scopes(s, set).unwrap();
    }
    // Without the set the host resolver reports every binding.
    let bare = validate_text_story(
        &p.stories[0],
        Budget::default(),
        &CancellationToken::default(),
        &Host,
    )
    .unwrap();
    assert!(!bare.is_empty());
    // Vertical alignment and auto-resize have no type style home: one loss per story each.
    let c = losses(p);
    let texts = m.doc.nodes.iter().filter(|n| n.type_name == "TEXT").count() as u32;
    assert_eq!(
        c[&(
            "native.text.align_vertical".to_owned(),
            "text_layout_no_type_style_home"
        )],
        texts
    );
    assert_eq!(
        c[&(
            "native.text.auto_resize".to_owned(),
            "text_layout_no_type_style_home"
        )],
        texts
    );
    // Run boundaries: "Regular text + " Regular, "bold text" Bold.
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
        let s = set
            .style(r.style_ref.as_deref().unwrap())
            .expect("style in set");
        (r.start, r.end, s.font_style.clone().unwrap())
    };
    assert_eq!(style_of(0), (0, 15, "Regular".to_owned()));
    assert_eq!(style_of(1), (15, 24, "Bold".to_owned()));
}

#[test]
fn real_package_round_trip_of_folio_kinds() {
    for name in FIXTURES {
        let m = mapped(name);
        let p = &m.proposal;
        let entries = p.import_entries(&m.import, &m.bytes).unwrap();
        let held: Vec<_> = entries.iter().map(|e| e.to_opaque()).collect();
        let mut out = Vec::new();
        write_package_with(
            &p.snapshot,
            &MemoryAssetSource::new(),
            &Extras {
                quarantined: &held,
                previews: &[],
            },
            &WriteOptions::default(),
            &PackageLimits::default(),
            &CancellationToken::default(),
            &mut out,
        )
        .unwrap();
        let cancel = CancellationToken::default();
        let opened = read_package(
            &out,
            &PackageLimits::default(),
            Budget::default(),
            &Host,
            &cancel,
        )
        .unwrap();
        let snap = opened.snapshot().expect("editable document");
        assert_eq!(snap.document(), p.snapshot.document(), "{name}");
        let records =
            read_native_records(opened.import_bytes(ImportRole::Records).unwrap()).unwrap();
        assert_eq!(records.vector_records, p.vector_records, "{name}");
        assert_eq!(records.raster_records, p.raster_records, "{name}");
        assert_eq!(records.type_styles, p.type_styles, "{name}");
        assert_eq!(records.paths, p.paths, "{name}");
        // The reopened records validate against the reopened document.
        validate_vector_records(
            &records.vector_records,
            snap.document(),
            Some(&records.design),
            Budget::default(),
            &cancel,
        )
        .unwrap();
        validate_type_style_records(
            &records.type_styles,
            snap.document(),
            Budget::default(),
            &cancel,
            &Host,
        )
        .unwrap();
        // The image paint resolves against the reopened original entry: its bytes hash to the
        // recorded entry digest and the part inside hashes to the recorded content digest.
        let original = opened.import_bytes(ImportRole::Original).unwrap();
        assert_eq!(original, m.bytes.as_slice(), "{name}");
        for img in &records.vector_records.image_paints {
            let StudioImageSource::ImportOriginal {
                entry_digest,
                part,
                content_digest,
            } = &img.image
            else {
                panic!("import-original source")
            };
            assert_eq!(entry_digest.digest, sha(original));
            let container =
                open_fig(original, &Limits::default(), &CancellationToken::default()).unwrap();
            let held = container.part(part).expect("part in reopened original");
            assert_eq!(content_digest.digest, sha(&held.bytes));
        }
    }
}
