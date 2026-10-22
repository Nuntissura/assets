//! Every package asset a document and its typed import records reference (SMB-PACKAGE-MASK-TILES).
//!
//! One collector serves the writers (which assets to write), the reader (which assets must be
//! present) and the session (which bodies to keep, stage and serve). It walks the document's wire
//! form and every `import/records.<sha256>` entry that holds JSON, and takes:
//!
//! - every tile reference: an object whose `schema_id` is [`TILE_SCHEMA_ID`] (Folio `TileRef`; the
//!   token is required on every tile, so raster payload tiles, layer mask coverage, smart-filter
//!   entry and stack masks and any future tile plane are found without a list of record kinds);
//! - every records image source naming a package asset: an object whose `source_kind` is
//!   `package_asset` (Folio `StudioImageSource::PackageAsset`).
//!
//! Record entries are `import/records.<sha256>` and the session's typed document entries
//! (`studio/<token>.<sha256>`, [`DOCUMENT_ENTRY_PREFIX`]); other opaque entries are never
//! interpreted. Record entries that are not JSON are opaque to the package and contribute nothing; their
//! schemas belong to their producer. A tile or image object that does not decode as its Folio type,
//! or whose digest is not lowercase SHA-256 hex, is `invalid_digest`.
#![deny(clippy::arithmetic_side_effects)]

use crate::{
    error::PackageError,
    hashing::is_hex64,
    import::{ImportRole, parse_import_name},
    read::Quarantined,
};
use hsk_studio_folio as folio;
use serde_json::Value;
use std::collections::BTreeMap;

/// Name prefix of the session's typed document entries (`studio/<token>.<sha256>`; their tokens
/// and producers belong to `hsk-studio-session`). The package reads JSON ones as Folio records.
pub const DOCUMENT_ENTRY_PREFIX: &str = "studio/";

/// True for an entry whose body may hold Folio records: `import/records.*` or `studio/*`.
pub(crate) fn is_record_entry(name: &str) -> bool {
    matches!(parse_import_name(name), Ok(Some((ImportRole::Records, _)))) || name.starts_with(DOCUMENT_ENTRY_PREFIX)
}

/// The JSON bodies of the record entries among `held` (non-JSON bodies are skipped: opaque).
pub(crate) fn record_bodies(held: &[Quarantined]) -> impl Iterator<Item = (&str, Value)> {
    held.iter().filter(|e| is_record_entry(&e.name)).filter_map(|e| {
        serde_json::from_slice::<Value>(&e.bytes).ok().map(|v| (e.name.as_str(), v))
    })
}

/// `schema_id` token every Folio tile reference carries.
pub const TILE_SCHEMA_ID: &str = "hsk.studio.raster_tile@1";
/// `source_kind` token of a records image source that names a package asset.
pub const PACKAGE_ASSET_SOURCE_KIND: &str = "package_asset";

/// Where an asset is referenced from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AssetUse {
    /// A tile of a layer's `Payload::Raster`.
    RasterTile,
    /// Any other tile plane of the document (layer mask coverage, ...).
    DocumentPlane,
    /// A tile plane of an import records entry (smart-filter entry and stack masks, ...).
    RecordPlane,
    /// A records image source naming a package asset.
    RecordImage,
}

/// One referenced asset.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReferencedAsset {
    /// Every use, sorted, without repeats.
    pub uses: Vec<AssetUse>,
    /// Element size for the `shuffle_zstd` codec: known only when every reference is a tile whose
    /// format names the same 2/4/8-byte element.
    pub elem_size: Option<u8>,
}

impl ReferencedAsset {
    /// A raster payload tile: a package without it is not openable (`missing_asset`). Any other
    /// missing plane opens with a typed loss note (`LossReason::MissingAsset`).
    pub fn required(&self) -> bool {
        self.uses.contains(&AssetUse::RasterTile)
    }
}

/// Element size of a tile format whose last `_` token names a fixed-width sample.
pub(crate) fn element_size_for_format(format: &str) -> Option<u8> {
    match format.rsplit('_').next()? {
        "f32le" | "u32le" | "i32le" => Some(4),
        "f16le" | "u16le" | "i16le" => Some(2),
        "f64le" | "u64le" | "i64le" => Some(8),
        _ => None,
    }
}

#[derive(Default)]
struct Acc {
    uses: Vec<AssetUse>,
    /// `None` = no element size seen yet; `Some(x)` = the agreed size so far.
    elem: Option<Option<u8>>,
}

impl Acc {
    fn add(&mut self, used: AssetUse, elem: Option<u8>) {
        if !self.uses.contains(&used) {
            self.uses.push(used);
            self.uses.sort();
        }
        self.elem = match self.elem {
            None => Some(elem),
            Some(prev) if prev == elem => Some(prev),
            Some(_) => Some(None),
        };
    }
}

pub(crate) fn checked_digest(d: &folio::ContentDigest) -> Result<&str, PackageError> {
    if d.algorithm != "sha256" || !is_hex64(&d.digest) {
        return Err(PackageError::InvalidDigest);
    }
    Ok(&d.digest)
}

/// A reference found in a JSON tree.
enum Found {
    Tile { digest: String, elem: Option<u8> },
    Image { digest: String },
}

/// Walks one JSON tree, handing every tile reference and package-asset image source to `found`.
fn walk(root: &Value, found: &mut dyn FnMut(Found)) -> Result<(), PackageError> {
    let mut stack: Vec<&Value> = vec![root];
    while let Some(v) = stack.pop() {
        match v {
            Value::Array(items) => stack.extend(items.iter()),
            Value::Object(map) => {
                if map.get("schema_id").and_then(Value::as_str) == Some(TILE_SCHEMA_ID) {
                    let tile: folio::TileRef =
                        serde_json::from_value(v.clone()).map_err(|_| PackageError::InvalidDigest)?;
                    found(Found::Tile {
                        digest: checked_digest(&tile.content_digest)?.to_owned(),
                        elem: element_size_for_format(&tile.format),
                    });
                    continue;
                }
                if map.get("source_kind").and_then(Value::as_str) == Some(PACKAGE_ASSET_SOURCE_KIND) {
                    let source: folio::StudioImageSource =
                        serde_json::from_value(v.clone()).map_err(|_| PackageError::InvalidDigest)?;
                    if let folio::StudioImageSource::PackageAsset { content_digest } = &source {
                        found(Found::Image { digest: checked_digest(content_digest)?.to_owned() });
                    }
                    continue;
                }
                stack.extend(map.values());
            }
            _ => {}
        }
    }
    Ok(())
}

/// Every asset `doc` and the typed import records among `held` reference, by lowercase SHA-256
/// hex (see the module documentation).
pub fn referenced_assets(
    doc: &folio::StudioDocument,
    held: &[Quarantined],
) -> Result<BTreeMap<String, ReferencedAsset>, PackageError> {
    let mut out: BTreeMap<String, Acc> = BTreeMap::new();
    // Raster payload tiles, typed: the ones a package cannot open without. Counted per digest so
    // the wire walk below can tell them from other planes sharing the same bytes.
    let mut raster: BTreeMap<String, usize> = BTreeMap::new();
    for layer in &doc.layers {
        if let folio::Payload::Raster { tiles } = &layer.payload {
            for tile in tiles {
                let digest = checked_digest(&tile.content_digest)?;
                out.entry(digest.to_owned())
                    .or_default()
                    .add(AssetUse::RasterTile, element_size_for_format(&tile.format));
                let n = raster.entry(digest.to_owned()).or_default();
                *n = n.saturating_add(1);
            }
        }
    }
    // Every tile plane of the document from its wire form; the raster tiles found again there are
    // consumed against the typed count, the rest are other planes (layer masks, ...).
    let wire = serde_json::to_value(doc).map_err(|_| PackageError::InvalidDigest)?;
    walk(&wire, &mut |found| match found {
        Found::Tile { digest, elem } => {
            if let Some(n) = raster.get_mut(&digest).filter(|n| **n > 0) {
                *n = n.saturating_sub(1);
            } else {
                out.entry(digest).or_default().add(AssetUse::DocumentPlane, elem);
            }
        }
        Found::Image { digest } => out.entry(digest).or_default().add(AssetUse::RecordImage, None),
    })?;
    for (_, records) in record_bodies(held) {
        walk(&records, &mut |found| match found {
            Found::Tile { digest, elem } => out.entry(digest).or_default().add(AssetUse::RecordPlane, elem),
            Found::Image { digest } => out.entry(digest).or_default().add(AssetUse::RecordImage, None),
        })?;
    }
    Ok(out
        .into_iter()
        .map(|(digest, acc)| {
            (
                digest,
                ReferencedAsset {
                    uses: acc.uses,
                    elem_size: acc.elem.flatten(),
                },
            )
        })
        .collect())
}
