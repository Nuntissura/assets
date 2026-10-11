//! Bounded verification and reopen of a `.handshake` container, from a byte slice or from a
//! `Read + Seek` stream through the same strict implementation.
use crate::{
    error::PackageError,
    hashing::{check, hex, is_hex64, sha256_hex},
    limits::Limits,
    manifest::{ASSET_PREFIX, Codec, EntryKind, MANIFEST_NAME, Manifest, ManifestEntry},
    zip::{EntryMeta, SliceSource, Source, StreamSource, preflight_source, stream_range},
};
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio as folio;
use ruzstd::decoding::StreamingDecoder;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{self, Read, Seek},
};

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
    /// Verified, decoded asset bytes keyed by lowercase SHA-256 hex.
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

/// `io::Read` over one archive entry's stored bytes; failures keep their typed cause.
struct Section<'a> {
    src: &'a mut dyn Source,
    pos: u64,
    end: u64,
    cancel: &'a CancellationToken,
    failure: Option<PackageError>,
}

impl Read for Section<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if let Err(e) = check(self.cancel) {
            self.failure = Some(e);
            return Err(io::Error::other("canceled"));
        }
        let n = (buf.len() as u64).min(self.end - self.pos) as usize;
        if n == 0 {
            return Ok(0);
        }
        match self.src.read_at(self.pos, &mut buf[..n]) {
            Ok(()) => {
                self.pos += n as u64;
                Ok(n)
            }
            Err(e) => {
                self.failure = Some(e);
                Err(io::Error::other("source"))
            }
        }
    }
}

type Sink<'a> = &'a mut dyn FnMut(&[u8]) -> Result<(), PackageError>;

/// Decodes exactly one zstd frame into `sink`, never producing more than `raw_len` bytes, never
/// declaring a window above `max_window`, and rejecting trailing bytes after the frame.
fn decode_zstd(
    src: &mut dyn Source,
    start: u64,
    size: u64,
    raw_len: u64,
    max_window: u64,
    cancel: &CancellationToken,
    sink: Sink<'_>,
) -> Result<(), PackageError> {
    let mut section = Section {
        src,
        pos: start,
        end: start + size,
        cancel,
        failure: None,
    };
    let result = decode_frame(&mut section, raw_len, max_window, cancel, sink);
    match result {
        Ok(()) if section.pos != section.end => Err(PackageError::Malformed("zstd_trailing_bytes")),
        Ok(()) => Ok(()),
        Err(PackageError::DecodeFailed) => {
            Err(section.failure.take().unwrap_or(PackageError::DecodeFailed))
        }
        Err(e) => Err(e),
    }
}

fn decode_frame(
    section: &mut Section<'_>,
    raw_len: u64,
    max_window: u64,
    cancel: &CancellationToken,
    sink: Sink<'_>,
) -> Result<(), PackageError> {
    let mut decoder = StreamingDecoder::new_with_max_window_size(&mut *section, max_window)
        .map_err(|_| PackageError::DecodeFailed)?;
    let mut buf = vec![0u8; 64 * 1024];
    let mut produced = 0u64;
    loop {
        check(cancel)?;
        let n = decoder
            .read(&mut buf)
            .map_err(|_| PackageError::DecodeFailed)?;
        if n == 0 {
            break;
        }
        produced += n as u64;
        if produced > raw_len {
            return Err(PackageError::LengthMismatch);
        }
        sink(&buf[..n])?;
    }
    if produced != raw_len {
        return Err(PackageError::LengthMismatch);
    }
    Ok(())
}

/// Preflighted container: structure, CRCs and manifest are valid; entry bodies are read (and
/// hash-verified) on demand in bounded chunks.
pub(crate) struct Container<S: Source> {
    src: S,
    limits: Limits,
    manifest: Manifest,
    metas: Vec<EntryMeta>,
    index: BTreeMap<String, usize>,
}

impl<S: Source> Container<S> {
    pub(crate) fn open(
        mut src: S,
        limits: &Limits,
        cancel: &CancellationToken,
    ) -> Result<Self, PackageError> {
        let metas = preflight_source(&mut src, limits, cancel)?;
        let first = metas
            .iter()
            .min_by_key(|m| m.header_offset)
            .ok_or(PackageError::MissingEntry)?;
        if first.name != MANIFEST_NAME {
            return Err(PackageError::Malformed("manifest_not_first"));
        }
        if first.size > limits.max_manifest_bytes {
            return Err(PackageError::EntryTooLarge);
        }
        let mut bytes = vec![0u8; first.size as usize];
        src.read_at(first.data_offset(), &mut bytes)?;
        let manifest = Manifest::parse(&bytes, limits)?;
        let index: BTreeMap<String, usize> = metas
            .iter()
            .enumerate()
            .map(|(i, m)| (m.name.clone(), i))
            .collect();
        for entry in &manifest.entries {
            let i = *index.get(&entry.name).ok_or(PackageError::MissingEntry)?;
            if metas[i].size != entry.len {
                return Err(PackageError::LengthMismatch);
            }
        }
        Ok(Self {
            src,
            limits: *limits,
            manifest,
            metas,
            index,
        })
    }

    pub(crate) fn into_source(self) -> S {
        self.src
    }

    pub(crate) fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    pub(crate) fn entries(&self) -> &[EntryMeta] {
        &self.metas
    }

    /// Streams (and decodes) one manifest entry, verifying its decoded SHA-256 identity.
    fn process_entry(
        &mut self,
        entry: &ManifestEntry,
        retain: bool,
        cancel: &CancellationToken,
    ) -> Result<Option<Vec<u8>>, PackageError> {
        let i = *self
            .index
            .get(&entry.name)
            .ok_or(PackageError::MissingEntry)?;
        let (start, size) = (self.metas[i].data_offset(), self.metas[i].size);
        let mut hasher = Sha256::new();
        let mut out: Option<Vec<u8>> = None;
        match entry.codec {
            Codec::Raw => {
                if retain {
                    out = Some(Vec::with_capacity(size as usize));
                }
                let mut sink = |chunk: &[u8]| -> Result<(), PackageError> {
                    hasher.update(chunk);
                    if let Some(o) = out.as_mut() {
                        o.extend_from_slice(chunk);
                    }
                    Ok(())
                };
                stream_range(&mut self.src, start, size, cancel, &mut sink)?;
            }
            Codec::Zstd => {
                let raw_len = entry.raw_len.ok_or(PackageError::ManifestInvalid)?;
                if retain {
                    out = Some(Vec::new());
                }
                let mut sink = |chunk: &[u8]| -> Result<(), PackageError> {
                    hasher.update(chunk);
                    if let Some(o) = out.as_mut() {
                        o.extend_from_slice(chunk);
                    }
                    Ok(())
                };
                decode_zstd(
                    &mut self.src,
                    start,
                    size,
                    raw_len,
                    self.limits.max_window_bytes,
                    cancel,
                    &mut sink,
                )?;
            }
        }
        if hex(hasher.finalize().as_slice()) != entry.sha256_hex {
            return Err(PackageError::HashMismatch);
        }
        Ok(out)
    }

    /// Reads and hash-verifies every manifest entry without retaining bodies.
    pub(crate) fn verify_all(&mut self, cancel: &CancellationToken) -> Result<(), PackageError> {
        let entries = self.manifest.entries.clone();
        for entry in &entries {
            check(cancel)?;
            self.process_entry(entry, false, cancel)?;
        }
        Ok(())
    }

    /// Decoded, verified bytes of one asset by SHA-256 hex.
    pub(crate) fn read_asset(
        &mut self,
        sha256_hex: &str,
        cancel: &CancellationToken,
    ) -> Result<Vec<u8>, PackageError> {
        let name = format!("{ASSET_PREFIX}{sha256_hex}");
        let entry = self
            .manifest
            .entries
            .iter()
            .find(|e| e.kind == EntryKind::Asset && e.name == name)
            .cloned()
            .ok_or(PackageError::MissingAsset)?;
        Ok(self
            .process_entry(&entry, true, cancel)?
            .unwrap_or_default())
    }

    pub(crate) fn into_opened(
        mut self,
        folio_budget: folio::Budget,
        resolver: &dyn folio::Resolver,
        cancel: &CancellationToken,
    ) -> Result<Opened, PackageError> {
        check(cancel)?;
        let entries = self.manifest.entries.clone();
        let mut document: Option<Vec<u8>> = None;
        let mut assets = BTreeMap::new();
        let mut held: Vec<(Quarantined, LossReason)> = Vec::new();
        let mut listed: BTreeSet<String> = BTreeSet::new();
        for entry in &entries {
            listed.insert(entry.name.clone());
            if entry.kind == EntryKind::Document && entry.len > folio_budget.input_bytes as u64 {
                return Err(PackageError::Document(folio::Diagnostic {
                    code: folio::Code::BudgetExceeded,
                    target: "input".to_owned(),
                }));
            }
            let data = self.process_entry(entry, true, cancel)?.unwrap_or_default();
            let reason = match entry.kind {
                EntryKind::Document => {
                    document = Some(data);
                    continue;
                }
                EntryKind::Asset => {
                    let hash = entry.name.strip_prefix(ASSET_PREFIX).unwrap_or_default();
                    assets.insert(hash.to_owned(), data);
                    continue;
                }
                EntryKind::Preview => LossReason::Preview,
                EntryKind::Opaque => LossReason::Opaque,
            };
            held.push((
                Quarantined {
                    name: entry.name.clone(),
                    bytes: data,
                    sha256_hex: entry.sha256_hex.clone(),
                },
                reason,
            ));
        }
        for i in 0..self.metas.len() {
            check(cancel)?;
            let name = self.metas[i].name.clone();
            if name == MANIFEST_NAME || listed.contains(&name) {
                continue;
            }
            let (start, size) = (self.metas[i].data_offset(), self.metas[i].size);
            let mut bytes = Vec::with_capacity(size as usize);
            stream_range(&mut self.src, start, size, cancel, &mut |chunk| {
                bytes.extend_from_slice(chunk);
                Ok(())
            })?;
            let sha256_hex = sha256_hex(&bytes, cancel)?;
            held.push((
                Quarantined {
                    name,
                    bytes,
                    sha256_hex,
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

        let document = document.ok_or(PackageError::MissingEntry)?;
        let inspection = folio::inspect_bytes(&document, folio_budget, cancel, resolver)
            .map_err(PackageError::Document)?;
        if let folio::Inspection::Editable(snapshot) = &inspection {
            let doc = snapshot.document();
            if doc.document_id != self.manifest.document_id
                || doc.revision != self.manifest.revision
            {
                return Err(PackageError::DocumentMismatch);
            }
            for digest in tile_digests(doc)? {
                if !assets.contains_key(&digest) {
                    return Err(PackageError::MissingAsset);
                }
            }
        }
        Ok(Opened {
            manifest: self.manifest,
            inspection,
            assets,
            quarantined,
            loss,
        })
    }
}

/// Structure, CRC, manifest and per-entry length/SHA-256 verification; no folio decode.
pub(crate) fn verify_container(
    bytes: &[u8],
    limits: &Limits,
    cancel: &CancellationToken,
) -> Result<(), PackageError> {
    Container::open(SliceSource(bytes), limits, cancel)?.verify_all(cancel)
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
    Container::open(SliceSource(bytes), limits, cancel)?.into_opened(folio_budget, resolver, cancel)
}

/// As [`read_package`] over a `Read + Seek` stream: the same strict checks, with reads bounded
/// to one chunk, one central-directory record, the manifest and the entry being materialised.
pub fn read_package_stream<R: Read + Seek>(
    src: R,
    limits: &Limits,
    folio_budget: folio::Budget,
    resolver: &dyn folio::Resolver,
    cancel: &CancellationToken,
) -> Result<Opened, PackageError> {
    PackageReader::open(src, limits, cancel)?.into_opened(folio_budget, resolver, cancel)
}

/// Lazy reader over a `Read + Seek` source: open verifies structure, CRCs and the manifest;
/// assets are decoded and hash-verified one at a time on request.
pub struct PackageReader<R: Read + Seek> {
    inner: Container<StreamSource<R>>,
}

impl<R: Read + Seek> PackageReader<R> {
    pub fn open(src: R, limits: &Limits, cancel: &CancellationToken) -> Result<Self, PackageError> {
        Ok(Self {
            inner: Container::open(StreamSource::new(src)?, limits, cancel)?,
        })
    }

    pub fn manifest(&self) -> &Manifest {
        self.inner.manifest()
    }

    pub fn entries(&self) -> &[EntryMeta] {
        self.inner.entries()
    }

    /// Reads and hash-verifies every manifest entry in bounded chunks, retaining nothing.
    pub fn verify_all(&mut self, cancel: &CancellationToken) -> Result<(), PackageError> {
        self.inner.verify_all(cancel)
    }

    /// Decoded, hash-verified bytes of one asset (lowercase SHA-256 hex).
    pub fn read_asset(
        &mut self,
        sha256_hex: &str,
        cancel: &CancellationToken,
    ) -> Result<Vec<u8>, PackageError> {
        self.inner.read_asset(sha256_hex, cancel)
    }

    pub fn into_opened(
        self,
        folio_budget: folio::Budget,
        resolver: &dyn folio::Resolver,
        cancel: &CancellationToken,
    ) -> Result<Opened, PackageError> {
        self.inner.into_opened(folio_budget, resolver, cancel)
    }

    pub fn into_inner(self) -> R {
        self.inner.into_source().into_inner()
    }
}
