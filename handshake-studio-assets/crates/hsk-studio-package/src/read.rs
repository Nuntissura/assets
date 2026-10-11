//! Bounded verification and reopen of a `.handshake` container.
use crate::{
    error::PackageError,
    hashing::{check, is_hex64, sha256_hex},
    limits::Limits,
    manifest::{ASSET_PREFIX, DOCUMENT_NAME, EntryKind, MANIFEST_NAME, Manifest},
    zip::{EntryMeta, preflight},
};
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio as folio;
use std::collections::{BTreeMap, BTreeSet};

/// Entry kept verbatim and never interpreted (STU-IO-187 quarantine).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Quarantined {
    pub name: String,
    pub bytes: Vec<u8>,
    pub sha256_hex: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LossReason {
    /// Present in the archive but not listed in the manifest.
    Unlisted,
    /// Listed with kind `opaque`: preserved, not semantically imported.
    Opaque,
    /// Listed with kind `preview`: preserved, not interpreted by this slice.
    Preview,
}

/// Explicit loss receipt: preserved-opaque, not semantically imported (STU-IO-004/005).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LossNote {
    pub name: String,
    pub reason: LossReason,
}

#[derive(Clone, Debug)]
pub struct Opened {
    pub manifest: Manifest,
    pub inspection: folio::Inspection,
    /// Verified asset bytes keyed by lowercase SHA-256 hex.
    pub assets: BTreeMap<String, Vec<u8>>,
    pub quarantined: Vec<Quarantined>,
    pub loss: Vec<LossNote>,
}

impl Opened {
    /// The editable document, when folio accepted it as editable.
    pub fn snapshot(&self) -> Option<&folio::Snapshot> {
        match &self.inspection {
            folio::Inspection::Editable(s) => Some(s),
            folio::Inspection::ReadOnly(_) => None,
        }
    }
}

/// Structure, CRC, manifest and per-entry length/SHA-256 verification; no folio decode.
pub(crate) fn verify_container(
    bytes: &[u8],
    limits: &Limits,
    cancel: &CancellationToken,
) -> Result<(Manifest, Vec<EntryMeta>), PackageError> {
    let metas = preflight(bytes, limits, cancel)?;
    let first = metas
        .iter()
        .min_by_key(|m| m.header_offset)
        .ok_or(PackageError::MissingEntry)?;
    if first.name != MANIFEST_NAME {
        return Err(PackageError::Malformed("manifest_not_first"));
    }
    let manifest = Manifest::parse(first.data(bytes), limits)?;
    let by_name: BTreeMap<&str, &EntryMeta> = metas.iter().map(|m| (m.name.as_str(), m)).collect();
    for entry in &manifest.entries {
        check(cancel)?;
        let meta = by_name
            .get(entry.name.as_str())
            .ok_or(PackageError::MissingEntry)?;
        if meta.size != entry.len {
            return Err(PackageError::LengthMismatch);
        }
        if sha256_hex(meta.data(bytes), cancel)? != entry.sha256_hex {
            return Err(PackageError::HashMismatch);
        }
    }
    Ok((manifest, metas))
}

/// Every distinct SHA-256 asset digest a document's raster tiles reference.
pub(crate) fn tile_digests(doc: &folio::StudioDocument) -> Result<BTreeSet<String>, PackageError> {
    let mut digests = BTreeSet::new();
    for layer in &doc.layers {
        if let folio::Payload::Raster { tiles } = &layer.payload {
            for tile in tiles {
                let d = &tile.content_digest;
                if d.algorithm != "sha256" || !is_hex64(&d.digest) {
                    return Err(PackageError::InvalidDigest);
                }
                digests.insert(d.digest.clone());
            }
        }
    }
    Ok(digests)
}

/// Opens a package from bytes under hard limits.
///
/// Unknown `container_version` fails with [`PackageError::UnsupportedVersion`] before any decode.
/// The document goes through [`folio::inspect_bytes`] unchanged, so folio's duplicate-field,
/// unknown-schema (read-only) and budget logic is reused rather than copied.
pub fn read_package(
    bytes: &[u8],
    limits: &Limits,
    folio_budget: folio::Budget,
    resolver: &dyn folio::Resolver,
    cancel: &CancellationToken,
) -> Result<Opened, PackageError> {
    check(cancel)?;
    let (manifest, metas) = verify_container(bytes, limits, cancel)?;
    let by_name: BTreeMap<&str, &EntryMeta> = metas.iter().map(|m| (m.name.as_str(), m)).collect();
    let document = by_name
        .get(DOCUMENT_NAME)
        .ok_or(PackageError::MissingEntry)?
        .data(bytes);
    let inspection = folio::inspect_bytes(document, folio_budget, cancel, resolver)
        .map_err(PackageError::Document)?;

    let mut assets = BTreeMap::new();
    let mut held: Vec<(Quarantined, LossReason)> = Vec::new();
    let mut listed: BTreeSet<&str> = BTreeSet::new();
    for entry in &manifest.entries {
        listed.insert(entry.name.as_str());
        let data = by_name
            .get(entry.name.as_str())
            .ok_or(PackageError::MissingEntry)?
            .data(bytes);
        let reason = match entry.kind {
            EntryKind::Document => continue,
            EntryKind::Asset => {
                let hash = entry.name.strip_prefix(ASSET_PREFIX).unwrap_or_default();
                assets.insert(hash.to_owned(), data.to_vec());
                continue;
            }
            EntryKind::Preview => LossReason::Preview,
            EntryKind::Opaque => LossReason::Opaque,
        };
        held.push((
            Quarantined {
                name: entry.name.clone(),
                bytes: data.to_vec(),
                sha256_hex: entry.sha256_hex.clone(),
            },
            reason,
        ));
    }
    for meta in &metas {
        if meta.name == MANIFEST_NAME || listed.contains(meta.name.as_str()) {
            continue;
        }
        let data = meta.data(bytes);
        held.push((
            Quarantined {
                name: meta.name.clone(),
                bytes: data.to_vec(),
                sha256_hex: sha256_hex(data, cancel)?,
            },
            LossReason::Unlisted,
        ));
    }
    held.sort_by(|a, b| a.0.name.cmp(&b.0.name));
    let (quarantined, loss) = held
        .into_iter()
        .map(|(q, reason)| {
            let note = LossNote {
                name: q.name.clone(),
                reason,
            };
            (q, note)
        })
        .unzip();

    if let folio::Inspection::Editable(snapshot) = &inspection {
        let doc = snapshot.document();
        if doc.document_id != manifest.document_id || doc.revision != manifest.revision {
            return Err(PackageError::DocumentMismatch);
        }
        for digest in tile_digests(doc)? {
            if !assets.contains_key(&digest) {
                return Err(PackageError::MissingAsset);
            }
        }
    }
    Ok(Opened {
        manifest,
        inspection,
        assets,
        quarantined,
        loss,
    })
}
