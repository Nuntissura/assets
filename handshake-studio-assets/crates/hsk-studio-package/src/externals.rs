//! References from a document and its records to data outside the document JSON (lane
//! SMB-PACKAGE-COMPLETENESS): every one is kept on save and reported on read with its link state,
//! so nothing outside the package is lost silently.
//!
//! Spec rules (STU-ASSET-006..009, STU-VID-038, STU-LAY-132): bulk binary (video and audio media,
//! LUTs, profiles, fonts) lives in the artifact tier and a document holds a LINK that resolves at
//! render, export and publish time; EMBEDDING copies the bytes into the document's own artifact
//! stream and is a per-reference choice; a link resolves to one of the closed states `resolved`,
//! `modified`, `missing`, `unauthorized`, `unresolved` (never collapsed); offline media keeps every
//! edit and relinks by content hash. Folio has no `placement_kind` field yet, so for a
//! content-addressed reference the package records the choice structurally: embedded = the
//! package carries `assets/<sha256>` of its digest (written under
//! [`crate::EmbedExternal::Granted`] / [`crate::EmbedExternal::All`]); otherwise linked.
//!
//! Kinds (found in the document wire form and every record entry, by their Folio discriminators):
//!
//! | kind | Folio shape | identity for relink | state from |
//! |---|---|---|---|
//! | `timeline_media` | `ClipSource::Media` (`source_kind: media`) | manifest id + content hash | embedded asset, else `Resolver::media` |
//! | `stored_story` | `StoryContent::Stored` (`content_kind: stored`) | manifest id + content hash | embedded asset, else `Resolver::story_content` |
//! | `import_original_part` | `StudioImageSource::ImportOriginal` | entry hash + part + part hash | the `import/original.<entry hash>` entry |
//! | `primitive` | any `schema_id` + `object_id` (`Payload::PrimitiveReference`, `ClipSource::Primitive`, `StudioEffectFill`) | schema id + object id | `Resolver::primitive` |
//! | `colour_profile` | `colour_profile_id`, `profile_ref`, working / blending spaces | `SCPF` id | `Resolver::profile` |
//! | `colour_lookup` | `StudioAdjustment::ColourLookup` (`lut`, `lut_kind`) | `lut` reference | `table` carried by value = embedded, else `Resolver::lut` |
//! | `colour_lookup` | clip-finishing `StudioLutRef` (exactly `artifact_manifest_id` + `content_digest`) | manifest id + content hash | embedded asset, else `Resolver::lut` |
//! | `font` | type style `font_family` / `font_style`, template control `font_family` / `locked_font`, motion template `required_fonts` | family + style | `Resolver::font` |
//! | `composition_audio` | motion `StudioAudioLayer.audio_id` | artifact id | `Resolver::audio` |
//! | `placed_source` | raster `StudioPlacedSource.location` (`mode: embedded`) | source document id + content hash | embedded asset, else missing |
//! | `placed_source` | raster `StudioPlacedSource.location` (`mode: linked`) | relative uri + content hash | `Resolver::placed_link` (exists, hash matches; [`crate::LinkedFileResolver`] checks the file next to the opened document) |
//! | `book_chapter` | book `StudioBookChapter` (chapter `SDOC` id, optional `file_path`) | file path, else document id | `Resolver::primitive("hsk.studio.document@1", id)` |
//! | `icc_profile` | vector `StudioIccProfile` (bytes "travel with the document") | `SCPF` id + content hash | embedded asset, else `Resolver::profile` |
//!
//! Embedded by the record's own intent (an `embedded` placed source, an ICC profile): a writer
//! embeds their bytes under every policy when the asset source grants them (under
//! [`crate::EmbedExternal::All`] they must be granted). Linked by default (timeline media, stored
//! story text): embedded only under `Granted` / `All`.
//!
//! A reference whose state is `missing`, `unauthorized`, `modified` or `unsupported` adds a loss
//! note ([`crate::LossReason::UnavailableExternal`]) named by [`ExternalRef::relink_key`]; the
//! matching [`ExternalRef`] carries the relink information.
#![deny(clippy::arithmetic_side_effects)]

use crate::{
    error::PackageError,
    hashing::is_hex64,
    import::{IMPORT_PREFIX, ImportRole},
    read::Quarantined,
    references::record_bodies,
};
use hsk_studio_folio as folio;
use serde_json::Value;
use std::collections::BTreeMap;

/// What an external reference points at.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ExternalKind {
    TimelineMedia,
    StoredStory,
    ImportOriginalPart,
    Primitive,
    ColourProfile,
    ColourLookup,
    Font,
    CompositionAudio,
    PlacedSource,
    IccProfile,
    BookChapter,
}

impl ExternalKind {
    pub const fn token(self) -> &'static str {
        match self {
            Self::TimelineMedia => "timeline_media",
            Self::StoredStory => "stored_story",
            Self::ImportOriginalPart => "import_original_part",
            Self::Primitive => "primitive",
            Self::ColourProfile => "colour_profile",
            Self::ColourLookup => "colour_lookup",
            Self::Font => "font",
            Self::CompositionAudio => "composition_audio",
            Self::PlacedSource => "placed_source",
            Self::IccProfile => "icc_profile",
            Self::BookChapter => "book_chapter",
        }
    }
}

/// Link state (STU-ASSET-007, closed; `embedded` is the STU-LAY-132 `link.embedded` status, and
/// `unsupported` is Folio's disposition for a format the host cannot use).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LinkState {
    /// The bytes travel in the package (`assets/<sha256>`, the import-original entry, or a LUT
    /// cube carried by value).
    Embedded,
    /// Linked; the caller's resolver has it with the recorded hash.
    Resolved,
    /// Linked; the resolver has it but the hash differs.
    Modified,
    /// Linked; nothing resolves it.
    Missing,
    /// Linked; the resolver denies this caller.
    Unauthorized,
    /// Linked; the resolver cannot use its format.
    Unsupported,
    /// Linked; no resolver port exists for this kind, so resolution is not attempted here.
    Unresolved,
}

impl LinkState {
    pub const fn token(self) -> &'static str {
        match self {
            Self::Embedded => "embedded",
            Self::Resolved => "resolved",
            Self::Modified => "modified",
            Self::Missing => "missing",
            Self::Unauthorized => "unauthorized",
            Self::Unsupported => "unsupported",
            Self::Unresolved => "unresolved",
        }
    }

    /// The states that make the referenced data unavailable to this reader (a loss note).
    pub const fn is_unavailable(self) -> bool {
        matches!(self, Self::Missing | Self::Unauthorized | Self::Modified | Self::Unsupported)
    }

    fn of(r: folio::Resolution) -> Self {
        match r {
            folio::Resolution::Available => Self::Resolved,
            folio::Resolution::Absent => Self::Missing,
            folio::Resolution::HashMismatch => Self::Modified,
            folio::Resolution::Unauthorized => Self::Unauthorized,
            folio::Resolution::Unsupported => Self::Unsupported,
        }
    }
}

/// One distinct external reference with what a relink needs.
#[derive(Clone, Debug, PartialEq)]
pub struct ExternalRef {
    pub kind: ExternalKind,
    /// The link handle: artifact manifest id, `schema_id/object_id`, `SCPF` id, LUT reference,
    /// `family/style`, audio artifact id, or `part` of an import original.
    pub handle: String,
    /// Content hash the link was made with (relink by hash), when the kind records one.
    pub content_digest: Option<folio::ContentDigest>,
    pub state: LinkState,
    /// Where the first occurrence sits: `document` or the entry name, then `#` and a JSON pointer.
    pub target: String,
    /// How many places reference it.
    pub occurrences: u32,
    /// CKC catalog record (`PRIM-Asset` handle) the reference names, for relink by catalog id
    /// (media clips and placed sources; lane SMB-PACKAGE-COMPLETENESS-2).
    pub asset_id: Option<String>,
}

impl ExternalRef {
    /// Stable loss-note name: `external/<kind>/<sha256 or handle>`.
    pub fn relink_key(&self) -> String {
        let id = match &self.content_digest {
            Some(d) if d.algorithm == "sha256" && self.kind != ExternalKind::ImportOriginalPart => d.digest.clone(),
            _ => self.handle.clone(),
        };
        format!("external/{}/{id}", self.kind.token())
    }
}

/// The content-addressed externals a writer may embed ([`crate::EmbedExternal`]), sorted: the
/// lowercase sha256 hex and whether the record itself says the bytes travel with the document (an
/// `embedded` placed source, an ICC profile) rather than by link (timeline media, stored story).
pub fn embeddable_digests(
    doc: &folio::StudioDocument,
    held: &[Quarantined],
) -> Result<Vec<(String, bool)>, PackageError> {
    let mut out: BTreeMap<String, bool> = BTreeMap::new();
    for_each_reference(doc, held, &mut |found| {
        let intent = match found.kind {
            ExternalKind::TimelineMedia | ExternalKind::StoredStory | ExternalKind::ColourLookup => false,
            ExternalKind::IccProfile => true,
            ExternalKind::PlacedSource if found.embedded_mode => true,
            _ => return,
        };
        if let Some(d) = &found.digest
            && d.algorithm == "sha256"
            && is_hex64(&d.digest)
        {
            let slot = out.entry(d.digest.clone()).or_insert(intent);
            *slot = *slot || intent;
        }
    })?;
    Ok(out.into_iter().collect())
}

/// A placed source's free-form digest label as a content digest: `sha256` when it is (optionally
/// `sha256-` / `sha256:` prefixed) lowercase 64-hex, else algorithm `label` with the text kept.
fn placed_digest(label: &str) -> folio::ContentDigest {
    let hex = label.strip_prefix("sha256-").or_else(|| label.strip_prefix("sha256:")).unwrap_or(label);
    if is_hex64(hex) {
        folio::ContentDigest { algorithm: "sha256".into(), digest: hex.into() }
    } else {
        folio::ContentDigest { algorithm: "label".into(), digest: label.into() }
    }
}

struct Found {
    kind: ExternalKind,
    handle: String,
    schema_object: Option<(String, String)>,
    digest: Option<folio::ContentDigest>,
    /// Import-original entry hash (kind `ImportOriginalPart`).
    entry_digest: Option<String>,
    /// A LUT cube carried by value (kind `ColourLookup`).
    carried: bool,
    /// A placed source in `embedded` mode (kind `PlacedSource`).
    embedded_mode: bool,
    /// `asset_id` member of the reference object.
    asset_id: Option<String>,
    target: String,
}

/// Schema id a book chapter document is resolved under (`Resolver::primitive`).
const BOOK_CHAPTER_SCHEMA: &str = "hsk.studio.document@1";

const PROFILE_KEYS: [&str; 7] = [
    "colour_profile_id",
    "profile_ref",
    "working_space_rgb",
    "working_space_cmyk",
    "working_space_gray",
    "working_space_spot",
    "blending_space",
];

fn digest_of(v: Option<&Value>) -> Option<folio::ContentDigest> {
    serde_json::from_value(v?.clone()).ok()
}

fn str_of<'a>(map: &'a serde_json::Map<String, Value>, key: &str) -> Option<&'a str> {
    map.get(key).and_then(Value::as_str)
}

/// JSON-pointer token escaping (RFC 6901).
fn escape(token: &str) -> String {
    token.replace('~', "~0").replace('/', "~1")
}

fn walk(root: &Value, origin: &str, found: &mut dyn FnMut(Found)) {
    let mut stack: Vec<(&Value, String)> = vec![(root, String::new())];
    while let Some((v, at)) = stack.pop() {
        match v {
            Value::Array(items) => {
                for (i, item) in items.iter().enumerate().rev() {
                    stack.push((item, format!("{at}/{i}")));
                }
            }
            Value::Object(map) => {
                let target = format!("{origin}#{at}");
                let tag = |k: &str| str_of(map, k);
                if tag("source_kind") == Some("media") {
                    found(Found {
                        kind: ExternalKind::TimelineMedia,
                        handle: str_of(map, "artifact_manifest_id").unwrap_or_default().to_owned(),
                        schema_object: None,
                        digest: digest_of(map.get("content_digest")),
                        entry_digest: None,
                        carried: false,
                        embedded_mode: false,
                        asset_id: str_of(map, "asset_id").map(str::to_owned),
                        target: target.clone(),
                    });
                }
                if tag("content_kind") == Some("stored") {
                    found(Found {
                        kind: ExternalKind::StoredStory,
                        handle: str_of(map, "artifact_manifest_id").unwrap_or_default().to_owned(),
                        schema_object: None,
                        digest: digest_of(map.get("content_digest")),
                        entry_digest: None,
                        carried: false,
                        embedded_mode: false,
                        asset_id: None,
                        target: target.clone(),
                    });
                }
                if tag("source_kind") == Some("import_original") {
                    found(Found {
                        kind: ExternalKind::ImportOriginalPart,
                        handle: str_of(map, "part").unwrap_or_default().to_owned(),
                        schema_object: None,
                        digest: digest_of(map.get("content_digest")),
                        entry_digest: digest_of(map.get("entry_digest")).map(|d| d.digest),
                        carried: false,
                        embedded_mode: false,
                        asset_id: None,
                        target: target.clone(),
                    });
                }
                // `Payload::PrimitiveReference`, `ClipSource::Primitive`, `StudioEffectFill`: a typed
                // primitive named by schema id and object id.
                if let (Some(_), Some(_)) = (str_of(map, "schema_id"), str_of(map, "object_id")) {
                    let (s, o) = (str_of(map, "schema_id").unwrap_or_default(), str_of(map, "object_id").unwrap_or_default());
                    found(Found {
                        kind: ExternalKind::Primitive,
                        handle: format!("{s}/{o}"),
                        schema_object: Some((s.to_owned(), o.to_owned())),
                        digest: None,
                        entry_digest: None,
                        carried: false,
                        embedded_mode: false,
                        asset_id: None,
                        target: target.clone(),
                    });
                }
                if let (Some(lut), Some(_)) = (str_of(map, "lut"), map.get("lut_kind")) {
                    found(Found {
                        kind: ExternalKind::ColourLookup,
                        handle: lut.to_owned(),
                        schema_object: None,
                        digest: None,
                        entry_digest: None,
                        carried: map.get("table").is_some_and(|t| !t.is_null()),
                        embedded_mode: false,
                        asset_id: None,
                        target: target.clone(),
                    });
                }
                if let Some(family) = str_of(map, "font_family") {
                    let style = str_of(map, "font_style").unwrap_or_default();
                    found(Found {
                        kind: ExternalKind::Font,
                        handle: format!("{family}/{style}"),
                        schema_object: None,
                        digest: None,
                        entry_digest: None,
                        carried: false,
                        embedded_mode: false,
                        asset_id: None,
                        target: target.clone(),
                    });
                }
                if let Some(audio) = str_of(map, "audio_id") {
                    found(Found {
                        kind: ExternalKind::CompositionAudio,
                        handle: audio.to_owned(),
                        schema_object: None,
                        digest: None,
                        entry_digest: None,
                        carried: false,
                        embedded_mode: false,
                        asset_id: None,
                        target: target.clone(),
                    });
                }
                // `StudioPlacedSource`: its `location` (`StudioPlacedMode`, tag `mode`) and catalog id.
                let location = map.get("location").and_then(Value::as_object);
                let loc = |k: &str| location.and_then(|l| l.get(k)).and_then(Value::as_str);
                if let (Some(mode @ ("embedded" | "linked")), Some(label)) = (loc("mode"), loc("content_digest")) {
                    let embedded_mode = mode == "embedded";
                    let handle = if embedded_mode { loc("document_id") } else { loc("uri") };
                    found(Found {
                        kind: ExternalKind::PlacedSource,
                        handle: handle.unwrap_or_default().to_owned(),
                        schema_object: None,
                        digest: Some(placed_digest(label)),
                        entry_digest: None,
                        carried: false,
                        embedded_mode,
                        asset_id: str_of(map, "asset_id").map(str::to_owned),
                        target: format!("{target}/location"),
                    });
                }
                // Clip-finishing `StudioLutRef` (STU-COL-158): exactly a manifest handle plus a digest.
                if map.len() == 2
                    && let (Some(handle), Some(digest)) =
                        (str_of(map, "artifact_manifest_id"), digest_of(map.get("content_digest")))
                {
                    found(Found {
                        kind: ExternalKind::ColourLookup,
                        handle: handle.to_owned(),
                        schema_object: None,
                        digest: Some(digest),
                        entry_digest: None,
                        carried: false,
                        embedded_mode: false,
                        asset_id: None,
                        target: target.clone(),
                    });
                }
                // Template fonts: `locked_font` (exposed source-text control) and every name of a
                // motion template's `required_fonts`.
                let mut font = |family: &str, at: String| {
                    found(Found {
                        kind: ExternalKind::Font,
                        handle: format!("{family}/"),
                        schema_object: None,
                        digest: None,
                        entry_digest: None,
                        carried: false,
                        embedded_mode: false,
                        asset_id: None,
                        target: at,
                    });
                };
                if let Some(family) = str_of(map, "locked_font") {
                    font(family, format!("{target}/locked_font"));
                }
                if let Some(Value::Array(names)) = map.get("required_fonts") {
                    for (i, name) in names.iter().enumerate() {
                        if let Some(family) = name.as_str() {
                            font(family, format!("{target}/required_fonts/{i}"));
                        }
                    }
                }
                // `StudioBookChapter`: a chapter is another Studio document, at `file_path` when
                // the book records one.
                if let (Some(document), Some(_)) = (str_of(map, "document_id"), map.get("document_page_range").or(map.get("status"))) {
                    let file = str_of(map, "file_path");
                    found(Found {
                        kind: ExternalKind::BookChapter,
                        handle: file.unwrap_or(document).to_owned(),
                        schema_object: Some((BOOK_CHAPTER_SCHEMA.to_owned(), document.to_owned())),
                        digest: None,
                        entry_digest: None,
                        carried: false,
                        embedded_mode: false,
                        asset_id: None,
                        target: target.clone(),
                    });
                }
                // `StudioIccProfile`: profile bytes that travel with the document.
                if let (Some(profile), Some(_), Some(digest)) =
                    (str_of(map, "profile_id"), map.get("colour_space"), digest_of(map.get("content_digest")))
                {
                    found(Found {
                        kind: ExternalKind::IccProfile,
                        handle: profile.to_owned(),
                        schema_object: None,
                        digest: Some(digest),
                        entry_digest: None,
                        carried: false,
                        embedded_mode: false,
                        asset_id: None,
                        target: target.clone(),
                    });
                }
                for key in PROFILE_KEYS {
                    if let Some(id) = str_of(map, key) {
                        found(Found {
                            kind: ExternalKind::ColourProfile,
                            handle: id.to_owned(),
                            schema_object: None,
                            digest: None,
                            entry_digest: None,
                            carried: false,
                            embedded_mode: false,
                        asset_id: None,
                            target: format!("{target}/{key}"),
                        });
                    }
                }
                let mut children: Vec<(&String, &Value)> = map.iter().collect();
                children.reverse();
                for (k, child) in children {
                    if matches!(child, Value::Object(_) | Value::Array(_)) {
                        stack.push((child, format!("{at}/{}", escape(k))));
                    }
                }
            }
            _ => {}
        }
    }
}

fn for_each_reference(
    doc: &folio::StudioDocument,
    held: &[Quarantined],
    found: &mut dyn FnMut(Found),
) -> Result<(), PackageError> {
    let wire = serde_json::to_value(doc).map_err(|_| PackageError::InvalidDigest)?;
    walk(&wire, "document", found);
    for (name, records) in record_bodies(held) {
        walk(&records, name, found);
    }
    Ok(())
}

/// Every distinct external reference of `doc` and the record entries among `held`, with its link
/// state: `embedded(sha256)` says whether the package carries `assets/<sha256>`; `resolver` is the
/// caller's (Folio's resolver port). Sorted by kind, then handle.
pub fn external_references(
    doc: &folio::StudioDocument,
    held: &[Quarantined],
    embedded: &dyn Fn(&str) -> bool,
    resolver: &dyn folio::Resolver,
) -> Result<Vec<ExternalRef>, PackageError> {
    let originals: Vec<&str> = held
        .iter()
        .filter_map(|q| q.name.strip_prefix(IMPORT_PREFIX))
        .filter_map(|rest| rest.strip_prefix(ImportRole::Original.token()))
        .filter_map(|rest| rest.strip_prefix('.'))
        .collect();
    let mut out: BTreeMap<(ExternalKind, String, Option<String>), ExternalRef> = BTreeMap::new();
    for_each_reference(doc, held, &mut |f| {
        let key = (f.kind, f.handle.clone(), f.digest.as_ref().map(|d| format!("{}:{}", d.algorithm, d.digest)));
        if let Some(seen) = out.get_mut(&key) {
            seen.occurrences = seen.occurrences.saturating_add(1);
            return;
        }
        let sha = f.digest.as_ref().filter(|d| d.algorithm == "sha256" && is_hex64(&d.digest)).map(|d| d.digest.as_str());
        let state = match f.kind {
            ExternalKind::TimelineMedia | ExternalKind::StoredStory if sha.is_some_and(embedded) => LinkState::Embedded,
            ExternalKind::TimelineMedia | ExternalKind::StoredStory => match &f.digest {
                Some(d) if f.kind == ExternalKind::TimelineMedia => LinkState::of(resolver.media(&f.handle, d)),
                Some(d) => LinkState::of(resolver.story_content(&f.handle, d)),
                None => LinkState::Missing,
            },
            ExternalKind::ImportOriginalPart => {
                if f.entry_digest.as_deref().is_some_and(|e| originals.contains(&e)) {
                    LinkState::Embedded
                } else {
                    LinkState::Missing
                }
            }
            ExternalKind::Primitive => {
                let (s, o) = f.schema_object.clone().unwrap_or_default();
                LinkState::of(resolver.primitive(&s, &o))
            }
            ExternalKind::ColourProfile => LinkState::of(resolver.profile(&f.handle)),
            ExternalKind::ColourLookup if f.carried => LinkState::Embedded,
            ExternalKind::ColourLookup if sha.is_some_and(embedded) => LinkState::Embedded,
            ExternalKind::ColourLookup => LinkState::of(resolver.lut(&f.handle, f.digest.as_ref())),
            ExternalKind::Font => {
                let (family, style) = f.handle.split_once('/').unwrap_or((f.handle.as_str(), ""));
                LinkState::of(resolver.font(family, style))
            }
            ExternalKind::CompositionAudio => LinkState::of(resolver.audio(&f.handle)),
            ExternalKind::BookChapter => {
                let (schema, document) = f.schema_object.clone().unwrap_or_default();
                LinkState::of(resolver.primitive(&schema, &document))
            }
            ExternalKind::IccProfile if sha.is_some_and(embedded) => LinkState::Embedded,
            ExternalKind::IccProfile => LinkState::of(resolver.profile(&f.handle)),
            ExternalKind::PlacedSource if f.embedded_mode => {
                if sha.is_some_and(embedded) { LinkState::Embedded } else { LinkState::Missing }
            }
            // The live check at open replaces the health recorded at the last save.
            ExternalKind::PlacedSource => {
                let label = f.digest.as_ref().map(|d| d.digest.as_str()).unwrap_or_default();
                LinkState::of(resolver.placed_link(&f.handle, label))
            }
        };
        out.insert(
            key,
            ExternalRef {
                kind: f.kind,
                // An import-original part relinks by its entry hash plus part name (handle) and its
                // own hash (content_digest).
                handle: if f.kind == ExternalKind::ImportOriginalPart {
                    format!("{}/{}", f.entry_digest.unwrap_or_default(), f.handle)
                } else {
                    f.handle
                },
                content_digest: f.digest,
                state,
                target: f.target,
                occurrences: 1,
                asset_id: f.asset_id,
            },
        );
    })?;
    Ok(out.into_values().collect())
}
