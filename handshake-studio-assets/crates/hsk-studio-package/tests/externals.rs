//! SMB-PACKAGE-COMPLETENESS: every reference from a document and its records to data outside the
//! document JSON is kept on save (as a link, or embedded per `EmbedExternal`) and reported on read
//! with its link state; an unavailable one is a typed loss whose relink data (handle, content hash,
//! location) is in `Opened::externals`. The schema guard classifies every reference-like property
//! of every committed Folio schema.

use hsk_studio_accord::{CancellationToken, Ticks};
use hsk_studio_folio::{self as folio, *};
use hsk_studio_package::*;
use serde_json::{Value, json};
use std::{collections::BTreeSet, io::Cursor};

const F: u64 = 10_160_640_000;
/// A colour profile the host does not have.
const DEAD_PROFILE: &str = "SCPF-019abcde-0000-7000-8000-00000000dead";

/// Media `media-ok` resolves; media `media-hash` resolves with another hash; everything else
/// media is absent; stored stories are denied; one primitive is unsupported.
struct Host;
impl folio::Resolver for Host {
    fn primitive(&self, _: &str, object_id: &str) -> Resolution {
        if object_id == "obj-unsupported" { Resolution::Unsupported } else { Resolution::Available }
    }
    fn profile(&self, id: &str) -> Resolution {
        if id.ends_with("00000000dead") { Resolution::Absent } else { Resolution::Available }
    }
    fn tile(&self, _: &TileRef) -> Resolution {
        Resolution::Available
    }
    fn format_supported(&self, _: &str) -> bool {
        true
    }
    fn primitive_layer_binding(&self, _: &str, _: &str) -> bool {
        false
    }
    fn media(&self, id: &str, _: &ContentDigest) -> Resolution {
        match id {
            "media-ok" => Resolution::Available,
            "media-hash" => Resolution::HashMismatch,
            _ => Resolution::Absent,
        }
    }
    fn story_content(&self, _: &str, _: &ContentDigest) -> Resolution {
        Resolution::Unauthorized
    }
    fn font(&self, family: &str, style: &str) -> Resolution {
        if (family, style) == ("Inter", "Bold") { Resolution::Available } else { Resolution::Absent }
    }
    fn audio(&self, _: &str) -> Resolution {
        Resolution::Unauthorized
    }
    fn lut(&self, lut: &str, digest: Option<&ContentDigest>) -> Resolution {
        match (lut, digest) {
            ("lut-artifact-film", None) => Resolution::HashMismatch,
            ("grade-look", Some(_)) => Resolution::Available,
            _ => Resolution::Absent,
        }
    }
    fn placed_link(&self, uri: &str, _: &str) -> Resolution {
        if uri == "links/logo.psd" { Resolution::Absent } else { Resolution::Available }
    }
}

/// A canonical CKC catalog asset id (kernel-bindings `AssetId` grammar).
const ASSET: &str = "0190f5c2-7a1b-7c3d-8e4f-000000000abc";

fn token() -> CancellationToken {
    CancellationToken::default()
}

fn id(prefix: &str, n: u8) -> String {
    format!("{prefix}-019abcde-0000-7000-8000-{n:012x}")
}

fn sha(bytes: &[u8]) -> ContentDigest {
    ContentDigest { algorithm: "sha256".into(), digest: sha256_hex_of(bytes) }
}

fn clip(n: u32, source: ClipSource, start: u64) -> StudioClip {
    StudioClip {
        schema_id: ClipSchema::V1,
        clip_id: format!("SCLP-019abcde-0000-7000-8000-{n:012x}"),
        source_ref: source,
        source_in: Ticks::new(0),
        source_out: Ticks::new(10 * F),
        timeline_start: Ticks::new(start * F),
        duration: Ticks::new(10 * F),
        speed: Speed::UNIT,
        enabled: true,
        link_group: None,
        markers: Vec::new(),
        transitions: ClipTransitions::default(),
    }
}

/// The external data of the fixture.
struct Bytes {
    embedded_media: Vec<u8>,
    story: Vec<u8>,
    original: Vec<u8>,
    part: Vec<u8>,
    placed_source: Vec<u8>,
    icc: Vec<u8>,
}

fn bytes() -> Bytes {
    Bytes {
        embedded_media: b"media bytes that travel inside the package".to_vec(),
        story: "stored story text".as_bytes().to_vec(),
        original: b"PK original container bytes".to_vec(),
        part: b"image part inside the original".to_vec(),
        placed_source: b"placed Studio source document package".to_vec(),
        icc: b"acsp icc profile bytes".to_vec(),
    }
}

/// A document with a timeline: media clips (one the package can embed, one resolved, one whose
/// hash changed, one absent) and a primitive clip, plus a profile the host does not have.
fn document(b: &Bytes) -> folio::Snapshot {
    let media = |name: &str, digest: ContentDigest| ClipSource::Media {
        artifact_manifest_id: name.into(),
        asset_id: (name == "media-ok").then(|| ASSET.to_string()),
        content_digest: digest,
    };
    let seq = StudioSequence {
        schema_id: SequenceSchema::V1,
        sequence_id: "seq".into(),
        name: "main".into(),
        frame_rate: SequenceFrameRate { ticks_per_frame: F },
        duration: Ticks::new(50 * F),
        tracks: vec![StudioTrack {
            schema_id: TrackSchema::V1,
            track_id: id("STRK", 1),
            kind: TrackKind::Video,
            index: 0,
            name: "V1".into(),
            enabled: true,
            locked: false,
            sync_locked: true,
            targeted: true,
            muted: false,
            soloed: false,
            clips: vec![
                clip(1, media("media-embedded", sha(&b.embedded_media)), 0),
                clip(2, media("media-ok", sha(b"resolved elsewhere")), 10),
                clip(3, media("media-hash", sha(b"old bytes")), 20),
                clip(4, media("media-gone", sha(b"never granted")), 30),
                clip(5, ClipSource::Primitive { schema_id: "hsk.studio.composition@1".into(), object_id: "obj-unsupported".into() }, 40),
            ],
            volume: 1.0,
            pan: 0.0,
        }],
        markers: Vec::new(),
        work_area: None,
    };
    let doc = json!({
        "schema_id":"hsk.studio.document@1","document_id":id("SDOC",1),"revision":2,"geometry_unit":"px","colour_mode":3,"bits_per_channel":8,
        "working_space_rgb":DEAD_PROFILE,"working_space_cmyk":null,"working_space_gray":null,"working_space_spot":null,"blending_space":null,
        "artboards":[],"page_spreads":[],
        "layers":[{"schema_id":"hsk.studio.layer@1","layer_id":id("SLYR",1),"name":"group","parent":null,"kind":"group","payload":{"payload_kind":"group"},"visible":true}],
        "graph":{"schema_id":"hsk.studio.layer_graph@1","operations":[],"edges":[],"outputs":[]},
        "sequences":[serde_json::to_value(seq).unwrap()]
    });
    match folio::inspect_bytes(&serde_json::to_vec(&doc).unwrap(), folio::Budget::default(), &token(), &Host).unwrap() {
        folio::Inspection::Editable(s) => s,
        folio::Inspection::ReadOnly(_) => panic!("fixture document became read-only"),
    }
}

/// A records entry with the record-side kinds, each serialized from its Folio type (or, for the
/// type style font and LUT shapes, the Folio wire keys).
fn records(b: &Bytes, original_sha: &str) -> ImportEntry {
    let stored = StoryContent::Stored { artifact_manifest_id: "story-artifact".into(), content_digest: sha(&b.story), byte_len: b.story.len() as u64 };
    let part = StudioImageSource::ImportOriginal {
        entry_digest: ContentDigest { algorithm: "sha256".into(), digest: original_sha.into() },
        part: "images/abc".into(),
        content_digest: sha(&b.part),
    };
    let lut_ref = StudioAdjustment::ColourLookup { lut: "lut-artifact-film".into(), lut_kind: StudioLutKind::Lut3d, dither: false, table: None };
    let lut_carried = StudioAdjustment::ColourLookup {
        lut: "lut-artifact-carried".into(),
        lut_kind: StudioLutKind::Lut3d,
        dither: false,
        table: Some(StudioLutTable { side: 2, values: vec![[0.0, 0.0, 0.0]; 8] }),
    };
    let audio = StudioAudioLayer { audio_id: "audio-artifact".into(), start_tick: 0, gain: 1.0 };
    let bundle = json!({
        "records":"bundle",
        "text_stories":[{"content": stored}],
        "vector_records":{"image_paints":[{"image": part}]},
        "raster_records":{"layers":[{"adjustment": lut_ref}, {"adjustment": lut_carried}]},
        "type_style_records":{"styles":[{"font_family":"Inter","font_style":"Bold"},{"font_family":"Inter","font_style":"Bold"}]},
        "motion":{"audio":[{"layers":[audio]}]},
        "effects":[{"pattern":{"schema_id":"hsk.studio.pattern@1","object_id":"pattern-7"}}],
        "book":{"chapters":[{"document_id":id("SDOC", 3),"status":"normal","file_path":"chapters/ch1.handshake"}]},
        "template":{"required_fonts":["Inter"],"controls":[{"control":{"locked_font":"Inter"}}]},
        "grade":{"look": serde_json::to_value(StudioLutRef { artifact_manifest_id: "grade-look".into(), content_digest: sha(b"look cube") }).unwrap()},
        "placed":{"placed_sources":[
            serde_json::to_value(StudioPlacedSource { source_id: "src-embedded".into(), location: StudioPlacedMode::Embedded { document_id: id("SDOC", 2), content_digest: format!("sha256-{}", sha256_hex_of(&b.placed_source)) }, width: 2.0, height: 2.0, uses: Vec::new(), asset_id: Some(ASSET.into()) }).unwrap(),
            serde_json::to_value(StudioPlacedSource { source_id: "src-linked".into(), location: StudioPlacedMode::Linked { uri: "links/logo.psd".into(), content_digest: "sha256-0011".into(), health: StudioLinkHealth::Missing }, width: 2.0, height: 2.0, uses: Vec::new(), asset_id: None }).unwrap()
        ]},
        "icc":{"icc_profiles":[
            serde_json::to_value(StudioIccProfile { profile_id: id("SCPF", 7), name: "press".into(), colour_space: StudioColorModel::Cmyk, content_digest: sha(&b.icc), working_space: true }).unwrap(),
            serde_json::to_value(StudioIccProfile { profile_id: id("SCPF", 8), name: "proof".into(), colour_space: StudioColorModel::Rgb, content_digest: sha(b"icc bytes not granted"), working_space: false }).unwrap()
        ]}
    });
    ImportEntry::new(ImportRole::Records, serde_json::to_vec(&bundle).unwrap())
}

fn held(b: &Bytes, with_original: bool) -> Vec<Quarantined> {
    let original = ImportEntry::new(ImportRole::Original, b.original.clone());
    let mut held = vec![records(b, &original.sha256_hex).to_opaque()];
    if with_original {
        held.push(original.to_opaque());
    }
    held.sort_by(|a, b| a.name.cmp(&b.name));
    held
}

fn write(snapshot: &folio::Snapshot, assets: &MemoryAssetSource, held: &[Quarantined], embed: EmbedExternal) -> Result<Vec<u8>, PackageError> {
    let extras = Extras { quarantined: held, previews: &[] };
    let options = WriteOptions { embed_external: embed, ..WriteOptions::default() };
    let mut buffered = Vec::new();
    let a = write_package_with(snapshot, assets, &extras, &options, &Limits::default(), &token(), &mut buffered);
    let mut streamed = Cursor::new(Vec::new());
    let b = write_package_streaming(snapshot, assets, &extras, &options, &Limits::default(), &token(), &mut streamed);
    match (a, b) {
        (Ok(_), Ok(_)) => {
            assert_eq!(streamed.into_inner(), buffered, "streaming and buffered writers agree");
            Ok(buffered)
        }
        (Err(a), Err(b)) => {
            assert_eq!(a, b, "both writers refuse alike");
            assert!(buffered.is_empty(), "no partial output");
            Err(a)
        }
        (a, b) => panic!("writers disagree: {a:?} / {b:?}"),
    }
}

fn open(bytes: &[u8]) -> Opened {
    read_package(bytes, &Limits::default(), folio::Budget::default(), &Host, &token()).unwrap()
}

fn states(opened: &Opened) -> Vec<(String, &'static str, &'static str, u32)> {
    opened.externals.iter().map(|e| (e.handle.clone(), e.kind.token(), e.state.token(), e.occurrences)).collect()
}

fn source(b: &Bytes) -> MemoryAssetSource {
    let mut m = MemoryAssetSource::new();
    m.insert(b.embedded_media.clone());
    m.insert(b.story.clone());
    m.insert(b.placed_source.clone());
    m.insert(b.icc.clone());
    m
}

#[test]
fn every_external_is_kept_on_save_and_reported_with_its_link_state() {
    let b = bytes();
    let snapshot = document(&b);
    let held = held(&b, true);
    let original_sha = sha256_hex_of(&b.original);

    // Default: links only. The document and records come back byte-identical, nothing embedded.
    let linked = write(&snapshot, &source(&b), &held, EmbedExternal::Link).unwrap();
    let opened = open(&linked);
    assert_eq!(opened.snapshot().unwrap().encoded_bytes(), snapshot.encoded_bytes(), "every link kept verbatim");
    assert_eq!(opened.quarantined, held, "records and original carried verbatim");
    // Bytes a record says travel with the document (embedded placed source, ICC profile) are
    // embedded under Link too; linked media and stored text are not.
    let embedded_now: BTreeSet<&String> = opened.assets.keys().collect();
    assert_eq!(embedded_now, [sha256_hex_of(&b.placed_source), sha256_hex_of(&b.icc)].iter().collect(), "Link embeds only travelling bytes");
    let profile_dead = DEAD_PROFILE.to_string();
    let mut wanted = vec![
        ("media-embedded".to_string(), "timeline_media", "missing", 1),
        ("media-gone".to_string(), "timeline_media", "missing", 1),
        ("media-hash".to_string(), "timeline_media", "modified", 1),
        ("media-ok".to_string(), "timeline_media", "resolved", 1),
        ("story-artifact".to_string(), "stored_story", "unauthorized", 1),
        (format!("{original_sha}/images/abc"), "import_original_part", "embedded", 1),
        ("hsk.studio.composition@1/obj-unsupported".to_string(), "primitive", "unsupported", 1),
        ("hsk.studio.pattern@1/pattern-7".to_string(), "primitive", "resolved", 1),
        (profile_dead.clone(), "colour_profile", "missing", 1),
        ("lut-artifact-carried".to_string(), "colour_lookup", "embedded", 1),
        ("grade-look".to_string(), "colour_lookup", "resolved", 1),
        ("lut-artifact-film".to_string(), "colour_lookup", "modified", 1),
        ("Inter/".to_string(), "font", "missing", 2),
        ("Inter/Bold".to_string(), "font", "resolved", 2),
        ("chapters/ch1.handshake".to_string(), "book_chapter", "resolved", 1),
        ("audio-artifact".to_string(), "composition_audio", "unauthorized", 1),
        (id("SDOC", 2), "placed_source", "embedded", 1),
        ("links/logo.psd".to_string(), "placed_source", "missing", 1),
        (id("SCPF", 7), "icc_profile", "embedded", 1),
        (id("SCPF", 8), "icc_profile", "resolved", 1),
    ];
    let kind_order = ["timeline_media", "stored_story", "import_original_part", "primitive", "colour_profile", "colour_lookup", "font", "composition_audio", "placed_source", "icc_profile", "book_chapter"];
    wanted.sort_by_key(|(h, k, _, _)| (kind_order.iter().position(|x| x == k), h.clone()));
    assert_eq!(states(&opened), wanted);

    // Unavailable ones are typed losses named by their relink key; relink data rides along.
    let lost: BTreeSet<String> = opened.loss.iter().filter(|n| n.reason == LossReason::UnavailableExternal).map(|n| n.name.clone()).collect();
    let expected: BTreeSet<String> = [
        format!("external/timeline_media/{}", sha256_hex_of(&b.embedded_media)),
        format!("external/timeline_media/{}", sha256_hex_of(b"never granted")),
        format!("external/timeline_media/{}", sha256_hex_of(b"old bytes")),
        format!("external/stored_story/{}", sha256_hex_of(&b.story)),
        "external/primitive/hsk.studio.composition@1/obj-unsupported".to_string(),
        format!("external/colour_profile/{profile_dead}"),
        "external/placed_source/links/logo.psd".to_string(),
        "external/colour_lookup/lut-artifact-film".to_string(),
        "external/composition_audio/audio-artifact".to_string(),
        "external/font/Inter/".to_string(),
    ]
    .into_iter()
    .collect();
    assert_eq!(lost, expected);
    // Catalog ids ride along for relink by catalog record.
    let catalog = |h: &str| opened.externals.iter().find(|e| e.handle == h).unwrap().asset_id.clone();
    assert_eq!(catalog("media-ok").as_deref(), Some(ASSET));
    assert_eq!(catalog(&id("SDOC", 2)).as_deref(), Some(ASSET));
    assert_eq!(catalog("media-gone"), None);
    let gone = opened.externals.iter().find(|e| e.handle == "media-gone").unwrap();
    assert_eq!(gone.content_digest.as_ref().unwrap().digest, sha256_hex_of(b"never granted"), "relink by content hash");
    assert_eq!(gone.target, "document#/sequences/0/tracks/0/clips/3/source_ref", "location of the reference");
    let story = opened.externals.iter().find(|e| e.kind == ExternalKind::StoredStory).unwrap();
    assert!(story.target.starts_with("import/records.") && story.target.ends_with("#/text_stories/0/content"), "{}", story.target);

    // Granted: what the source grants is embedded and reopens embedded; the rest stays linked.
    let granted = write(&snapshot, &source(&b), &held, EmbedExternal::Granted).unwrap();
    let opened = open(&granted);
    assert_eq!(opened.assets.get(&sha256_hex_of(&b.embedded_media)), Some(&b.embedded_media));
    assert_eq!(opened.assets.get(&sha256_hex_of(&b.story)), Some(&b.story));
    let state = |h: &str| opened.externals.iter().find(|e| e.handle == h).unwrap().state;
    assert_eq!(state("media-embedded"), LinkState::Embedded);
    assert_eq!(state("story-artifact"), LinkState::Embedded);
    assert_eq!(state("media-gone"), LinkState::Missing);
    // A re-save of the reopened package keeps the embedded copies (byte-identical package).
    let mut reopened_assets = MemoryAssetSource::new();
    for body in opened.assets.values() {
        reopened_assets.insert(body.clone());
    }
    let again = write(opened.snapshot().unwrap(), &reopened_assets, &opened.quarantined, EmbedExternal::Granted).unwrap();
    assert_eq!(again, granted);

    // All: a self-contained package needs every content-addressed external; a missing one fails
    // the save typed with nothing written.
    let err = write(&snapshot, &source(&b), &held, EmbedExternal::All).unwrap_err();
    assert_eq!(err.code(), "asset_absent");
}

#[test]
fn a_missing_import_original_is_a_typed_loss_with_its_relink_data() {
    let b = bytes();
    let snapshot = document(&b);
    let held = held(&b, false);
    let opened = open(&write(&snapshot, &source(&b), &held, EmbedExternal::Link).unwrap());
    let part = opened.externals.iter().find(|e| e.kind == ExternalKind::ImportOriginalPart).unwrap();
    assert_eq!(part.state, LinkState::Missing);
    assert_eq!(part.content_digest.as_ref().unwrap().digest, sha256_hex_of(&b.part));
    assert!(opened.loss.iter().any(|n| n.reason == LossReason::UnavailableExternal && n.name == part.relink_key()), "{:?}", opened.loss);
}

#[test]
fn hostile_external_shapes_are_bounded_and_never_crash() {
    let b = bytes();
    let snapshot = document(&b);
    // Malformed reference shapes (wrong types, missing members) are reported, never a panic; a
    // records entry deeper than the JSON parser's bound is opaque.
    let odd = json!({"a":[{"source_kind":"media","artifact_manifest_id":7,"content_digest":"x"},
        {"content_kind":"stored"},{"lut":"l","lut_kind":3,"table":null},{"font_family":1},{"audio_id":["x"]},
        {"colour_profile_id":{"nested":true}},{"payload_kind":"primitive_reference"}]});
    let deep = format!("{}{}", "[".repeat(100_000), "]".repeat(100_000)).into_bytes();
    for body in [serde_json::to_vec(&odd).unwrap(), deep, b"\xff not json".to_vec()] {
        let held = vec![ImportEntry::new(ImportRole::Records, body).to_opaque()];
        let refs = external_references(snapshot.document(), &held, &|_| false, &Host).unwrap();
        assert!(refs.iter().all(|r| !r.relink_key().is_empty()));
        let package = write(&snapshot, &MemoryAssetSource::new(), &held, EmbedExternal::Granted).unwrap();
        let opened = open(&package);
        assert_eq!(opened.quarantined, held, "the entry is carried verbatim");
    }
}

/// Every reference-like property of every committed Folio wire schema, classified. A schema that
/// gains a property whose name says it points outside the document (digest, artifact, manifest,
/// asset, media, audio, font, lut, profile, path, uri/url, file, link, source, catalog, image,
/// story, object id, working/blending space) fails here until it is classified and, if it is a
/// dependency on outside data, covered by `references.rs` (package assets) or `externals.rs`.
#[test]
fn every_folio_reference_property_is_classified() {
    let schemas: Vec<(&str, Value)> = vec![
        ("document", serde_json::to_value(folio::document_schema()).unwrap()),
        ("raster_records", serde_json::to_value(folio::raster_records_schema()).unwrap()),
        ("vector_records", serde_json::to_value(folio::vector_records_schema()).unwrap()),
        ("design_records", serde_json::to_value(folio::design_records_schema()).unwrap()),
        ("composition_records", serde_json::to_value(folio::composition_records_schema()).unwrap()),
        ("motion_records", serde_json::to_value(folio::motion_records_schema()).unwrap()),
        ("layout_flow_records", serde_json::to_value(folio::layout_flow_records_schema()).unwrap()),
        ("live_shapes", serde_json::to_value(folio::live_shapes_schema()).unwrap()),
        ("longdoc_records", serde_json::to_value(folio::longdoc_records_schema()).unwrap()),
        ("parent_page_records", serde_json::to_value(folio::parent_page_records_schema()).unwrap()),
        ("text_story", serde_json::to_value(folio::text_story_schema()).unwrap()),
        ("type_style_records", serde_json::to_value(folio::type_style_records_schema()).unwrap()),
        ("vector_network", serde_json::to_value(folio::vector_network_schema()).unwrap()),
        ("book", serde_json::to_value(folio::book_schema()).unwrap()),
        ("clip_finishing", serde_json::to_value(folio::clip_finishing_schema()).unwrap()),
        ("layout_grids", serde_json::to_value(folio::layout_grids_schema()).unwrap()),
        ("multicam_records", serde_json::to_value(folio::multicam_records_schema()).unwrap()),
        ("proof_records", serde_json::to_value(folio::proof_records_schema()).unwrap()),
        ("design_state", serde_json::to_value(folio::design_state_records_schema()).unwrap()),
        ("layout_rules", serde_json::to_value(folio::layout_rule_records_schema()).unwrap()),
        ("cjk_composition", serde_json::to_value(folio::cjk_composition_records_schema()).unwrap()),
    ];
    const VOCAB: [&str; 22] = [
        "digest", "artifact", "manifest", "asset", "media", "audio", "font", "lut", "profile", "path", "uri", "url", "file",
        "link", "source", "catalog", "image", "story", "object_id", "working_space", "blending_space", "external",
    ];
    let mut found: BTreeSet<String> = BTreeSet::new();
    for (_, schema) in &schemas {
        let defs = schema.get("$defs").and_then(Value::as_object).cloned().unwrap_or_default();
        let mut roots: Vec<(String, &Value)> = vec![(schema["title"].as_str().unwrap_or("root").to_string(), schema)];
        roots.extend(defs.iter().map(|(k, v)| (k.clone(), v)));
        for (def, body) in roots {
            let mut stack: Vec<&Value> = vec![body];
            while let Some(v) = stack.pop() {
                match v {
                    Value::Object(map) => {
                        if let Some(Value::Object(props)) = map.get("properties") {
                            for prop in props.keys() {
                                if VOCAB.iter().any(|w| prop.contains(w)) {
                                    found.insert(format!("{def}.{prop}"));
                                }
                            }
                        }
                        stack.extend(map.iter().filter(|(k, _)| k.as_str() != "$defs").map(|(_, v)| v));
                    }
                    Value::Array(items) => stack.extend(items.iter()),
                    _ => {}
                }
            }
        }
    }
    let found: Vec<String> = found.into_iter().collect();
    let classified: Vec<&str> = CLASSIFIED.iter().map(|(p, _)| *p).collect();
    assert_eq!(found, classified, "unclassified reference properties in Folio schemas");
    let kinds = [
        ExternalKind::TimelineMedia, ExternalKind::StoredStory, ExternalKind::ImportOriginalPart, ExternalKind::Primitive,
        ExternalKind::ColourProfile, ExternalKind::ColourLookup, ExternalKind::Font, ExternalKind::CompositionAudio,
        ExternalKind::PlacedSource, ExternalKind::IccProfile, ExternalKind::BookChapter,
    ];
    for (prop, class) in CLASSIFIED {
        for part in class.split(' ') {
            if let Some(kind) = part.strip_prefix("external:") {
                assert!(kinds.iter().any(|k| k.token() == kind), "{prop}: {kind} is not an ExternalKind the package reports");
            }
        }
    }
}

/// `(Def.property, class)`: `asset` = package asset (references.rs), `external:<kind>` = link
/// reported by externals.rs, `internal` = a document-local id or value, `not_a_dependency` = data
/// that names no input (a destination, a documentation URL, a flag).
const CLASSIFIED: &[(&str, &str)] = &[
    ("ClipSource.artifact_manifest_id", "external:timeline_media"),
    ("ClipSource.asset_id", "external:timeline_media (CKC catalog id)"),
    ("ClipSource.content_digest", "external:timeline_media"),
    ("ClipSource.object_id", "external:primitive"),
    ("ClipSource.source_kind", "discriminator: timeline_media or primitive"),
    ("ContentDigest.digest", "member of the classified reference that holds it"),
    ("Edge.source", "internal: graph port"),
    ("Payload.object_id", "external:primitive"),
    ("Payload.path_ids", "internal: vector path records"),
    ("Payload.story_id", "internal: text story records"),
    ("StoryContent.artifact_manifest_id", "external:stored_story"),
    ("StoryContent.content_digest", "external:stored_story"),
    ("StudioAction.url", "not_a_dependency: prototype navigation target"),
    ("StudioAdjustment.lut", "external:colour_lookup"),
    ("StudioAdjustment.lut_kind", "discriminator: colour_lookup"),
    ("StudioAdjustment.source", "internal: match-colour statistics by value"),
    ("StudioAnchoredObject.story_id", "internal: text story records"),
    ("StudioAudioLayer.audio_id", "external:composition_audio"),
    ("StudioBlendRange.source_black", "internal: value"),
    ("StudioBlendRange.source_white", "internal: value"),
    ("StudioBook.style_source_document", "internal: one of the book's chapter ids"),
    ("StudioBookChapter.file_path", "external:book_chapter"),
    ("StudioBookSync.sync_composite_fonts", "internal: flag"),
    ("StudioBooleanGroup.fill_geometry_path_ids", "internal: vector path records"),
    ("StudioBrushArt.path_id", "internal: vector path records"),
    ("StudioClip.link_group", "internal: clip link group"),
    ("StudioClip.source_in", "internal: value"),
    ("StudioClip.source_out", "internal: value"),
    ("StudioClip.source_ref", "container of ClipSource"),
    ("StudioClipEffectParams.input_lut", "container of StudioLutRef"),
    ("StudioColor.colour_profile_id", "external:colour_profile"),
    ("StudioColourTarget.path", "internal: shape item path"),
    ("StudioConditionalRange.story_id", "internal: text story records"),
    ("StudioControlKind.font_family_editable", "internal: flag"),
    ("StudioControlKind.font_size_editable", "internal: flag"),
    ("StudioControlKind.locked_font", "external:font"),
    ("StudioControlSetting.font_family", "external:font"),
    ("StudioControlSetting.font_size", "internal: value"),
    ("StudioCrossReference.source_end", "internal: value"),
    ("StudioCrossReference.source_start", "internal: value"),
    ("StudioCrossReference.source_story_id", "internal: text story records"),
    ("StudioDocument.blending_space", "external:colour_profile"),
    ("StudioDocument.working_space_cmyk", "external:colour_profile"),
    ("StudioDocument.working_space_gray", "external:colour_profile"),
    ("StudioDocument.working_space_rgb", "external:colour_profile"),
    ("StudioDocument.working_space_spot", "external:colour_profile"),
    ("StudioDocumentationLink.url", "not_a_dependency: documentation link"),
    ("StudioEffectFill.object_id", "external:primitive"),
    ("StudioEffectKind.link_with_layer", "internal: flag"),
    ("StudioEffectKind.source", "internal: glow source enum"),
    ("StudioEndnote.note_story_id", "internal: text story records"),
    ("StudioEndnote.story_id", "internal: text story records"),
    ("StudioEnvelopeShape.source_layer_id", "internal: document layer"),
    ("StudioFill.link_with_layer", "internal: flag"),
    ("StudioFootnote.note_story_id", "internal: text story records"),
    ("StudioFootnote.story_id", "internal: text story records"),
    ("StudioFrameShape.path_id", "internal: vector path records"),
    ("StudioGradientMesh.source_path_id", "internal: vector path records"),
    ("StudioGridAlignment.story_id", "internal: text story records"),
    ("StudioIccProfile.content_digest", "external:icc_profile"),
    ("StudioIccProfile.profile_id", "external:icc_profile"),
    ("StudioIccProfile.working_space", "internal: flag"),
    ("StudioImagePaint.image", "container of StudioImageSource"),
    ("StudioImageSource.content_digest", "asset (package_asset) or external:import_original_part"),
    ("StudioImageSource.entry_digest", "external:import_original_part"),
    ("StudioImageSource.source_kind", "discriminator: asset or import_original_part"),
    ("StudioIndexMark.story_id", "internal: text story records"),
    ("StudioLayoutRuleRecords.linked_page_items", "internal: linked page item records (document layers)"),
    ("StudioLayoutRuleRecords.linked_stories", "internal: linked story records"),
    ("StudioLightOptions.colour_profile_id", "external:colour_profile"),
    ("StudioLinkedStory.child_story_id", "internal: text story records"),
    ("StudioLinkedStory.parent_story_id", "internal: text story records"),
    ("StudioLiveShape.path_id", "internal: vector path records"),
    ("StudioLutRef.artifact_manifest_id", "external:colour_lookup"),
    ("StudioLutRef.content_digest", "external:colour_lookup"),
    ("StudioMarker.url", "not_a_dependency: web-link marker"),
    ("StudioMask.linked", "internal: flag"),
    ("StudioMatchStatistics.source_mean", "internal: value"),
    ("StudioMatchStatistics.source_std", "internal: value"),
    ("StudioMotionRecords.composition_audio", "container of StudioAudioLayer"),
    ("StudioMotionTemplate.required_fonts", "external:font"),
    ("StudioMulticamClip.audio_angle", "internal: angle number of the multicam source sequence"),
    ("StudioMulticamClip.audio_follows_video", "internal: flag"),
    ("StudioObjectStyle.epub_aria_label_source_type", "internal: value"),
    ("StudioObjectStyleCategories.enable_story_options", "internal: flag"),
    ("StudioOutputModule.audio", "not_a_dependency: output audio settings"),
    ("StudioOutputModule.include_project_link", "not_a_dependency: flag"),
    ("StudioOutputModule.include_source_metadata", "not_a_dependency: flag"),
    ("StudioOutputModule.output_path", "not_a_dependency: render destination"),
    ("StudioParagraphFlow.story_id", "internal: text story records"),
    ("StudioPathText.path_effect", "internal: value"),
    ("StudioPathText.path_id", "internal: vector path records"),
    ("StudioPathText.path_layer_id", "internal: document layer"),
    ("StudioPathText.path_type_alignment", "internal: value"),
    ("StudioPathText.story_id", "internal: text story records"),
    ("StudioPattern.source", "internal: pattern source (layer or tile artwork)"),
    ("StudioPatternSource.source_layer_id", "internal: document layer"),
    ("StudioPlacedAsset.source_id", "internal: placed source id"),
    ("StudioPlacedMode.content_digest", "external:placed_source"),
    ("StudioPlacedMode.uri", "external:placed_source"),
    ("StudioPlacedSource.asset_id", "external:placed_source (CKC catalog id)"),
    ("StudioPlacedSource.source_id", "internal: placed source id"),
    ("StudioProofConditionRecord.proof_profile", "internal: proof device choice (shipped preset, ICC reference, colour vision, operator matrix)"),
    ("StudioProofProfile.content_digest", "internal: sha256 of the StudioIccProfile the reference names (the package reports its bytes through that record as icc_profile)"),
    ("StudioProofProfile.profile_id", "internal: SCPF id of a StudioIccProfile in the vector records"),
    ("StudioPublishing.documentation_links", "not_a_dependency: documentation links"),
    ("StudioRasterLayerRecord.placed_asset", "container of StudioPlacedAsset"),
    ("StudioRasterRecords.placed_sources", "container of StudioPlacedSource"),
    ("StudioReaction.media_hit_time", "internal: value"),
    ("StudioRenderSettings.resolution", "internal: value"),
    ("StudioRgba.colour_profile_id", "external:colour_profile"),
    ("StudioRotoEdgeStroke.path", "internal: value (stroke geometry points, not a file reference)"),
    ("StudioRotoStroke.path", "internal: value (stroke geometry points, not a file reference)"),
    ("StudioShapeParameters.create_compound_path_from_ellipses", "internal: flag"),
    ("StudioShapeStream.path", "internal: shape path index"),
    ("StudioSwatch.profile_ref", "external:colour_profile"),
    ("StudioTable.cell_story_id", "internal: text story records"),
    ("StudioTable.story_id", "internal: text story records"),
    ("StudioTextAnchor.story_id", "internal: text story records"),
    ("StudioTextSelector.source", "internal: expression text"),
    ("StudioTextStory.story_direction", "internal: value"),
    ("StudioTextStory.story_id", "internal: text story records"),
    ("StudioTrackMatte.source_layer_id", "internal: document layer"),
    ("StudioTypeStyle.font_family", "external:font"),
    ("StudioTypeStyle.font_size", "internal: value"),
    ("StudioTypeStyle.font_style", "external:font"),
    ("StudioVariableKind.include_path", "internal: flag"),
    ("StudioVectorClip.source_layer_id", "internal: document layer"),
    ("StudioVectorMask.source_layer_id", "internal: document layer"),
    ("StudioVectorMaskParameters.linked", "internal: flag"),
    ("StudioVectorNetwork.path_id", "internal: vector path records"),
    ("StudioVectorRecords.icc_profiles", "container of StudioIccProfile"),
    ("StudioVectorRecords.image_paints", "container of StudioImagePaint"),
    ("StudioVectorRecords.path_texts", "container of StudioPathText"),
    ("StudioVectorRecords.width_profiles", "internal: width profiles"),
    ("StudioWidthProfile.path_id", "internal: vector path records"),
    ("StudioXrefDestination.story_id", "internal: text story records"),
    ("TileRef.artifact_manifest_id", "asset (provenance handle of a package tile)"),
    ("TileRef.colour_profile_id", "external:colour_profile"),
    ("TileRef.content_digest", "asset"),
];
