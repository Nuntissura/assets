//! Lazy package source: the session keeps the opened package as a `Read + Seek` stream and pulls
//! asset bodies and previews from it on demand, one at a time, hash-verified by the package
//! reader. At rest the session holds the document, preserved unknown entries and nothing else.
use crate::error::SessionError;
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{self as folio, ContentDigest, Payload, Resolver, Snapshot, StudioDocument};
use hsk_studio_package::{
    ASSET_PREFIX, AssetError, AssetReader, AssetSource, AssetStream, Codec, EntryKind, EntryMeta,
    Limits, LossNote, LossReason, MANIFEST_NAME, Manifest, PackageError, PackageReader,
    Quarantined, sha256_hex_of,
};
use std::{
    collections::BTreeSet,
    io::{self, Cursor, Read, Seek, SeekFrom},
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicU64, Ordering},
    },
};

const CHUNK: usize = 1 << 20;

pub(crate) trait ReadSeekSend: Read + Seek + Send {}
impl<T: Read + Seek + Send> ReadSeekSend for T {}

pub(crate) type SourceBox = Box<dyn ReadSeekSend>;

struct Shared {
    inner: Mutex<SourceBox>,
    bytes_read: Arc<AtomicU64>,
}

/// An independent logical cursor over one shared stream: every call seeks to its own position
/// first, so two handles keep independent positions, and every byte read is counted (test
/// `lazy_source_holds_no_assets_and_save_streams_them_on_demand` asserts the byte counts).
pub(crate) struct Handle {
    shared: Arc<Shared>,
    pos: u64,
}

impl Handle {
    /// Two handles over one stream (the package reader and a raw reader for entries the
    /// package reader does not expose without loading assets).
    pub(crate) fn pair(source: SourceBox, bytes_read: Arc<AtomicU64>) -> (Handle, Handle) {
        let shared = Arc::new(Shared {
            inner: Mutex::new(source),
            bytes_read,
        });
        (
            Handle {
                shared: Arc::clone(&shared),
                pos: 0,
            },
            Handle { shared, pos: 0 },
        )
    }

    /// Another independent cursor over the same stream, positioned at the start.
    pub(crate) fn fork(&self) -> Handle {
        Handle {
            shared: Arc::clone(&self.shared),
            pos: 0,
        }
    }
}

impl Read for Handle {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut inner = self
            .shared
            .inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        inner.seek(SeekFrom::Start(self.pos))?;
        let n = inner.read(buf)?;
        self.pos = self.pos.saturating_add(n as u64);
        self.shared
            .bytes_read
            .fetch_add(n as u64, Ordering::Relaxed);
        Ok(n)
    }
}

impl Seek for Handle {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        let invalid = || io::Error::new(io::ErrorKind::InvalidInput, "seek out of range");
        self.pos = match from {
            SeekFrom::Start(p) => p,
            SeekFrom::Current(d) => self.pos.checked_add_signed(d).ok_or_else(invalid)?,
            SeekFrom::End(d) => {
                let mut inner = self
                    .shared
                    .inner
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                let end = inner.seek(SeekFrom::End(0))?;
                end.checked_add_signed(d).ok_or_else(invalid)?
            }
        };
        Ok(self.pos)
    }
}

fn read_range(
    handle: &mut Handle,
    meta: &EntryMeta,
    cancel: &CancellationToken,
) -> Result<Vec<u8>, PackageError> {
    let io_error = |e: io::Error| PackageError::Io(e.kind());
    let size = usize::try_from(meta.size).map_err(|_| PackageError::EntryTooLarge)?;
    handle
        .seek(SeekFrom::Start(meta.data_offset()))
        .map_err(io_error)?;
    let mut out = Vec::with_capacity(size);
    let mut chunk = vec![0u8; size.min(CHUNK)];
    while out.len() < size {
        cancel.check().map_err(|_| PackageError::Canceled)?;
        let n = size.saturating_sub(out.len()).min(chunk.len());
        handle.read_exact(&mut chunk[..n]).map_err(io_error)?;
        out.extend_from_slice(&chunk[..n]);
    }
    Ok(out)
}

/// What the session keeps resident after open.
pub(crate) struct Parts {
    pub(crate) snapshot: Snapshot,
    pub(crate) quarantined: Vec<Quarantined>,
    pub(crate) loss: Vec<LossNote>,
}

/// Reads the document and the entries that must be preserved verbatim (opaque and unlisted),
/// verifies them against the manifest, and checks that every raster tile's asset is present in
/// the manifest. Asset and preview bodies are not read here (test
/// `lazy_source_holds_no_assets_and_save_streams_them_on_demand`: open reads the file once).
pub(crate) fn load_parts(
    manifest: &Manifest,
    entries: &[EntryMeta],
    raw: &mut Handle,
    limits: &Limits,
    budget: folio::Budget,
    resolver: &dyn Resolver,
    cancel: &CancellationToken,
) -> Result<Parts, SessionError> {
    let wrap = SessionError::from_package;
    let meta_of = |name: &str| entries.iter().find(|m| m.name == name);
    let mut document: Option<Vec<u8>> = None;
    let mut held: Vec<(Quarantined, LossReason)> = Vec::new();
    let mut resident = 0u64;
    let mut listed: BTreeSet<&str> = BTreeSet::new();
    for entry in &manifest.entries {
        listed.insert(entry.name.as_str());
        match entry.kind {
            EntryKind::Asset | EntryKind::Preview => {}
            EntryKind::Document => {
                if entry.len > budget.input_bytes as u64 {
                    return Err(wrap(PackageError::Document(folio::Diagnostic {
                        code: folio::Code::BudgetExceeded,
                        target: "input".to_owned(),
                    })));
                }
                let meta = meta_of(&entry.name).ok_or(wrap(PackageError::MissingEntry))?;
                let bytes = read_range(raw, meta, cancel).map_err(wrap)?;
                if sha256_hex_of(&bytes) != entry.sha256_hex {
                    return Err(wrap(PackageError::HashMismatch));
                }
                document = Some(bytes);
            }
            EntryKind::Opaque => {
                resident = resident.saturating_add(entry.len);
                if resident > limits.max_entry_bytes {
                    return Err(wrap(PackageError::TotalTooLarge));
                }
                let meta = meta_of(&entry.name).ok_or(wrap(PackageError::MissingEntry))?;
                let bytes = read_range(raw, meta, cancel).map_err(wrap)?;
                if sha256_hex_of(&bytes) != entry.sha256_hex {
                    return Err(wrap(PackageError::HashMismatch));
                }
                held.push((
                    Quarantined {
                        name: entry.name.clone(),
                        bytes,
                        sha256_hex: entry.sha256_hex.clone(),
                    },
                    LossReason::Opaque,
                ));
            }
        }
    }
    for meta in entries {
        if meta.name == MANIFEST_NAME || listed.contains(meta.name.as_str()) {
            continue;
        }
        resident = resident.saturating_add(meta.size);
        if resident > limits.max_entry_bytes {
            return Err(wrap(PackageError::TotalTooLarge));
        }
        let bytes = read_range(raw, meta, cancel).map_err(wrap)?;
        held.push((
            Quarantined {
                name: meta.name.clone(),
                sha256_hex: sha256_hex_of(&bytes),
                bytes,
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

    let document = document.ok_or(wrap(PackageError::MissingEntry))?;
    let inspection = folio::inspect_bytes(&document, budget, cancel, resolver)
        .map_err(|d| wrap(PackageError::Document(d)))?;
    let snapshot = match inspection {
        folio::Inspection::Editable(snapshot) => snapshot,
        folio::Inspection::ReadOnly(_) => return Err(SessionError::ReadOnlyDocument),
    };
    let doc = snapshot.document();
    if doc.document_id != manifest.document_id || doc.revision != manifest.revision {
        return Err(wrap(PackageError::DocumentMismatch));
    }
    require_tile_assets(doc, manifest).map_err(wrap)?;
    Ok(Parts {
        snapshot,
        quarantined,
        loss,
    })
}

/// Every raster tile of `doc` names a sha256 asset that the package manifest lists; a document
/// the source cannot write assets for is refused here instead of failing later at save.
pub(crate) fn require_tile_assets(
    doc: &StudioDocument,
    manifest: &Manifest,
) -> Result<(), PackageError> {
    for layer in &doc.layers {
        if let Payload::Raster { tiles } = &layer.payload {
            for tile in tiles {
                let digest = &tile.content_digest;
                if digest.algorithm != "sha256" {
                    return Err(PackageError::InvalidDigest);
                }
                let name = format!("{ASSET_PREFIX}{}", digest.digest);
                let present = manifest
                    .entries
                    .iter()
                    .any(|e| e.kind == EntryKind::Asset && e.name == name);
                if !present {
                    return Err(PackageError::MissingAsset);
                }
            }
        }
    }
    Ok(())
}

/// Asset bytes pulled from the source package on demand, one asset per call, bounded by the
/// writer's `max`. `read` (buffered export) is hash-verified by the package reader; `open`
/// (streaming save) hands the writer a bounded reader over the stored bytes, which the writer
/// holds to the declared length and the SHA-256 identity while it copies. The real cause of a
/// failed read (cancel, hash mismatch, I/O) is kept so the caller can report it instead of
/// `asset_absent`.
pub(crate) struct LazyAssets<'a> {
    reader: &'a Mutex<PackageReader<Handle>>,
    /// A cursor over the same stream as `reader`, forked for each raw asset range.
    raw: &'a Handle,
    cancel: &'a CancellationToken,
    failure: Mutex<Option<PackageError>>,
}

impl<'a> LazyAssets<'a> {
    pub(crate) fn new(
        reader: &'a Mutex<PackageReader<Handle>>,
        raw: &'a Handle,
        cancel: &'a CancellationToken,
    ) -> Self {
        Self {
            reader,
            raw,
            cancel,
            failure: Mutex::new(None),
        }
    }

    fn fail(&self, error: PackageError) -> AssetError {
        *self.failure.lock().unwrap_or_else(PoisonError::into_inner) = Some(error);
        AssetError::Absent
    }

    pub(crate) fn take_failure(&self) -> Option<PackageError> {
        self.failure
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }
}

impl AssetSource for LazyAssets<'_> {
    fn read(&self, digest: &ContentDigest, max: u64) -> Result<Vec<u8>, AssetError> {
        if digest.algorithm != "sha256" {
            return Err(AssetError::Absent);
        }
        let mut reader = self.reader.lock().unwrap_or_else(PoisonError::into_inner);
        let name = format!("{ASSET_PREFIX}{}", digest.digest);
        let declared = reader
            .manifest()
            .entries
            .iter()
            .find(|e| e.kind == EntryKind::Asset && e.name == name)
            .map(|e| e.raw_len.unwrap_or(e.len));
        match declared {
            None => return Err(AssetError::Absent),
            Some(size) if size > max => return Err(AssetError::TooLarge),
            Some(_) => {}
        }
        reader
            .read_asset(&digest.digest, self.cancel)
            .map_err(|error| self.fail(error))
    }
}

impl AssetStream for LazyAssets<'_> {
    /// A raw asset streams straight from its byte range in the source (constant memory); a
    /// zstd or shuffle_zstd asset is decoded and verified as one blob. Repeatable: the writer
    /// opens a codec asset once to plan and once to write, and only one reader is open at a time.
    fn open(&self, digest: &ContentDigest, max: u64) -> Result<AssetReader<'_>, AssetError> {
        if digest.algorithm != "sha256" {
            return Err(AssetError::Absent);
        }
        let name = format!("{ASSET_PREFIX}{}", digest.digest);
        let mut reader = self.reader.lock().unwrap_or_else(PoisonError::into_inner);
        let Some((codec, stored, declared)) = reader
            .manifest()
            .entries
            .iter()
            .find(|e| e.kind == EntryKind::Asset && e.name == name)
            .map(|e| (e.codec, e.len, e.raw_len.unwrap_or(e.len)))
        else {
            return Err(AssetError::Absent);
        };
        if declared > max {
            return Err(AssetError::TooLarge);
        }
        if codec == Codec::Raw {
            let Some(meta) = reader.entries().iter().find(|m| m.name == name) else {
                return Err(self.fail(PackageError::MissingEntry));
            };
            if meta.size != stored {
                return Err(self.fail(PackageError::LengthMismatch));
            }
            let mut range = self.raw.fork();
            if let Err(error) = range.seek(SeekFrom::Start(meta.data_offset())) {
                return Err(self.fail(PackageError::Io(error.kind())));
            }
            return Ok(AssetReader {
                len: stored,
                reader: Box::new(range.take(stored)),
            });
        }
        match reader.read_asset(&digest.digest, self.cancel) {
            Ok(bytes) => Ok(AssetReader {
                len: bytes.len() as u64,
                reader: Box::new(Cursor::new(bytes)),
            }),
            Err(error) => Err(self.fail(error)),
        }
    }
}
