//! Streaming package writer: asset bodies flow from an [`AssetStream`] into a seekable sink in
//! bounded chunks, so peak memory does not grow with asset size.
//!
//! Two passes, because `manifest.json` is physically first and states every entry's stored length:
//!
//! 1. **Plan** (no bytes written): validate names/limits and fix each entry's codec and stored
//!    length. Raw assets cost nothing here (the declared stream length is the stored length); a
//!    codec-compressed asset is read and encoded once, one blob at a time, and only its
//!    `(codec, stored length)` is kept.
//! 2. **Write**: manifest, document, then the remaining entries by name, exactly as
//!    [`crate::write_package_with`] orders them. A raw asset is copied through one reusable chunk
//!    buffer while its SHA-256 and CRC-32 are computed; its local-header CRC field is patched
//!    afterwards (no data descriptors, so the canonical-form reader accepts the output unchanged).
//!    A compressed asset is read and encoded again (encoding is deterministic) and must reproduce
//!    the planned stored length.
//!
//! The output is byte-identical to the buffered writer for the same inputs. After a failure the
//! sink holds a partial archive: only content faults found while streaming (hash mismatch, length
//! mismatch, I/O, cancel) can leave one, since all structural checks run in the plan pass. Callers
//! that need an all-or-nothing file use [`crate::save_atomic_streaming`].
use crate::{
    error::{AssetError, PackageError},
    hashing::{check, hex, sha256_hex},
    limits::Limits,
    manifest::{
        ASSET_PREFIX, CONTAINER_VERSION, Codec, DOCUMENT_NAME, EntryKind, MANIFEST_NAME, Manifest,
        ManifestEntry, PREVIEW_PREFIX, PreviewInfo,
    },
    write::{
        AssetReader, AssetStream, Extras, WriteOptions, WriteReceipt, asset_plan, encode_blob,
    },
    zip::{ZipWriter, validate_name, validate_name_set},
};
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{ContentDigest, Snapshot};
use ruzstd::encoding::{CompressionLevel, compress_to_vec};
use sha2::{Digest, Sha256};
use std::{
    borrow::Cow,
    io::{self, Read, Seek, Write},
};

/// Chunk size of the streamed copy and of codec-blob reads; the only buffer a raw asset needs.
const CHUNK: usize = 1 << 20;

enum Body<'a> {
    /// Small bytes already in memory: document, preserved entries, previews.
    Memory(Cow<'a, [u8]>),
    /// Asset copied from the stream verbatim.
    Stream,
    /// Asset read and encoded again at write time (one blob buffered at a time).
    Encode { elem: Option<u8> },
}

struct Planned<'a> {
    name: String,
    kind: EntryKind,
    codec: Codec,
    raw_len: Option<u64>,
    elem_size: Option<u8>,
    preview: Option<PreviewInfo>,
    /// SHA-256 of the decoded bytes (content identity).
    sha: String,
    stored_len: u64,
    body: Body<'a>,
}

fn io_error(error: &io::Error) -> PackageError {
    PackageError::Io(error.kind())
}

fn open_asset<'s>(
    assets: &'s dyn AssetStream,
    digest: &str,
    limits: &Limits,
) -> Result<AssetReader<'s>, PackageError> {
    let reference = ContentDigest {
        algorithm: "sha256".to_owned(),
        digest: digest.to_owned(),
    };
    let handle = assets
        .open(&reference, limits.max_entry_bytes)
        .map_err(PackageError::Asset)?;
    if handle.len > limits.max_entry_bytes {
        return Err(PackageError::Asset(AssetError::TooLarge));
    }
    Ok(handle)
}

/// Reads exactly `handle.len` bytes into one buffer, verifying the digest. Bounded by
/// `limits.max_entry_bytes`; this is the one-blob buffer of the codec paths.
fn read_blob(
    mut handle: AssetReader<'_>,
    digest: &str,
    cancel: &CancellationToken,
) -> Result<Vec<u8>, PackageError> {
    let len = usize::try_from(handle.len).map_err(|_| PackageError::EntryTooLarge)?;
    let mut bytes = Vec::with_capacity(len);
    let wanted = handle.len.saturating_add(1);
    loop {
        check(cancel)?;
        let step = (wanted - bytes.len() as u64).min(CHUNK as u64);
        if step == 0 {
            break;
        }
        let n = (&mut handle.reader)
            .take(step)
            .read_to_end(&mut bytes)
            .map_err(|e| io_error(&e))?;
        if (n as u64) < step {
            break;
        }
    }
    if bytes.len() as u64 != handle.len {
        return Err(PackageError::LengthMismatch);
    }
    if sha256_hex(&bytes, cancel)? != digest {
        return Err(PackageError::HashMismatch);
    }
    Ok(bytes)
}

/// Copies one raw asset into the open zip entry, hashing as it goes.
fn copy_verified<W: Write + Seek>(
    zip: &mut ZipWriter<'_, W>,
    name: &str,
    digest: &str,
    mut handle: AssetReader<'_>,
    chunk: &mut [u8],
    cancel: &CancellationToken,
) -> Result<(), PackageError> {
    zip.begin_streamed(name, handle.len, cancel)?;
    let mut sha = Sha256::new();
    loop {
        check(cancel)?;
        let n = match handle.reader.read(chunk) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(io_error(&e)),
        };
        sha.update(&chunk[..n]);
        zip.write_streamed(&chunk[..n])?;
    }
    zip.end_streamed()?;
    if hex(sha.finalize().as_slice()) != digest {
        return Err(PackageError::HashMismatch);
    }
    Ok(())
}

/// As [`crate::write_package_with`], but asset bytes come from an [`AssetStream`] and the output
/// goes to a seekable sink positioned at its start. Peak memory is one chunk for raw assets and
/// one blob (plus its encoded copy) for codec-compressed ones, independent of the number and
/// total size of the assets; the output bytes are identical to the buffered writer's.
///
/// The sink must start at position 0 and receives a partial archive when this returns an error
/// after the first byte; use [`crate::save_atomic_streaming`] for an all-or-nothing file.
pub fn write_package_streaming<W: Write + Seek>(
    doc: &Snapshot,
    assets: &dyn AssetStream,
    extras: &Extras<'_>,
    options: &WriteOptions,
    limits: &Limits,
    cancel: &CancellationToken,
    out: &mut W,
) -> Result<WriteReceipt, PackageError> {
    check(cancel)?;
    if out.stream_position().map_err(|e| io_error(&e))? != 0 {
        return Err(PackageError::Io(io::ErrorKind::InvalidInput));
    }
    let document = doc.document();
    let doc_bytes = doc.encoded_bytes();

    // ---- plan -----------------------------------------------------------------------------
    let mut parts: Vec<Planned<'_>> = Vec::new();
    for (digest, elem, must) in asset_plan(document, extras.quarantined, options)? {
        check(cancel)?;
        let handle = match open_asset(assets, &digest, limits) {
            Ok(handle) => handle,
            Err(PackageError::Asset(AssetError::Absent)) if !must => continue,
            Err(e) => return Err(e),
        };
        let name = format!("{ASSET_PREFIX}{digest}");
        let raw_len = handle.len;
        if options.asset_codec == Codec::Raw {
            parts.push(Planned {
                name,
                kind: EntryKind::Asset,
                codec: Codec::Raw,
                raw_len: None,
                elem_size: None,
                preview: None,
                sha: digest,
                stored_len: raw_len,
                body: Body::Stream,
            });
            continue;
        }
        let bytes = read_blob(handle, &digest, cancel)?;
        let encoded = encode_blob(options.asset_codec, elem, bytes, cancel)?;
        let (codec, body) = match encoded.codec {
            Codec::Raw => (Codec::Raw, Body::Stream),
            other => (other, Body::Encode { elem }),
        };
        parts.push(Planned {
            name,
            kind: EntryKind::Asset,
            codec,
            raw_len: encoded.raw_len,
            elem_size: encoded.elem_size,
            preview: None,
            sha: digest,
            stored_len: encoded.stored.len() as u64,
            body,
        });
    }
    for held in extras.quarantined {
        let sha = sha256_hex(&held.bytes, cancel)?;
        crate::import::check_import(&held.name, &sha, held.bytes.len() as u64, limits)?;
        parts.push(Planned {
            name: held.name.clone(),
            kind: EntryKind::Opaque,
            codec: Codec::Raw,
            raw_len: None,
            elem_size: None,
            preview: None,
            sha,
            stored_len: held.bytes.len() as u64,
            body: Body::Memory(Cow::Borrowed(held.bytes.as_slice())),
        });
    }
    for image in extras.previews {
        check(cancel)?;
        if !image.info.valid(limits) || image.pixels.len() as u64 != image.info.byte_len() {
            return Err(PackageError::PreviewMismatch);
        }
        let sha = sha256_hex(&image.pixels, cancel)?;
        let plain = (options.asset_codec != Codec::Raw)
            .then(|| compress_to_vec(&image.pixels[..], CompressionLevel::Fastest))
            .filter(|plain| plain.len() < image.pixels.len());
        let (codec, raw_len, stored) = match plain {
            Some(plain) => (
                Codec::Zstd,
                Some(image.pixels.len() as u64),
                Cow::Owned(plain),
            ),
            None => (Codec::Raw, None, Cow::Borrowed(image.pixels.as_slice())),
        };
        parts.push(Planned {
            name: format!("{PREVIEW_PREFIX}{sha}"),
            kind: EntryKind::Preview,
            codec,
            raw_len,
            elem_size: None,
            preview: Some(image.info.clone()),
            sha,
            stored_len: stored.len() as u64,
            body: Body::Memory(stored),
        });
    }
    parts.push(Planned {
        name: DOCUMENT_NAME.to_owned(),
        kind: EntryKind::Document,
        codec: Codec::Raw,
        raw_len: None,
        elem_size: None,
        preview: None,
        sha: sha256_hex(doc_bytes, cancel)?,
        stored_len: doc_bytes.len() as u64,
        body: Body::Memory(Cow::Borrowed(doc_bytes)),
    });
    parts.sort_by(|a, b| a.name.cmp(&b.name));

    validate_name_set(
        parts
            .iter()
            .map(|p| p.name.as_str())
            .chain(std::iter::once(MANIFEST_NAME)),
    )?;

    let entries = parts
        .iter()
        .map(|p| ManifestEntry {
            name: p.name.clone(),
            kind: p.kind,
            sha256_hex: p.sha.clone(),
            len: p.stored_len,
            codec: p.codec,
            raw_len: p.raw_len,
            elem_size: p.elem_size,
            preview: p.preview.clone(),
        })
        .collect();
    let manifest = Manifest {
        container_version: CONTAINER_VERSION,
        document_id: document.document_id.clone(),
        revision: document.revision,
        entries,
    };
    let manifest_bytes = manifest.to_canonical_bytes()?;
    if manifest_bytes.len() as u64 > limits.max_manifest_bytes {
        return Err(PackageError::EntryTooLarge);
    }
    if parts.len() + 1 > limits.max_entries as usize {
        return Err(PackageError::TooManyEntries);
    }
    // Pre-validate everything the zip writer would check so structural failure writes no byte.
    let mut total = manifest_bytes.len() as u64;
    for part in &parts {
        validate_name(part.name.as_bytes(), limits.max_name_len)?;
        if part.stored_len > limits.max_entry_bytes {
            return Err(PackageError::EntryTooLarge);
        }
        total = total
            .checked_add(part.stored_len)
            .filter(|t| *t <= limits.max_total_bytes)
            .ok_or(PackageError::TotalTooLarge)?;
    }

    // ---- write ----------------------------------------------------------------------------
    let mut zip = ZipWriter::new(out, *limits, options.zip64);
    zip.add(MANIFEST_NAME, &manifest_bytes, cancel)?;
    let mut chunk = vec![0u8; CHUNK];
    // Physical order: manifest first, then document, then the remaining entries by name.
    let ordered = parts
        .iter()
        .filter(|p| p.name == DOCUMENT_NAME)
        .chain(parts.iter().filter(|p| p.name != DOCUMENT_NAME));
    for part in ordered {
        check(cancel)?;
        match &part.body {
            Body::Memory(bytes) => zip.add(&part.name, bytes, cancel)?,
            Body::Stream => {
                let handle = open_asset(assets, &part.sha, limits)?;
                if handle.len != part.stored_len {
                    return Err(PackageError::LengthMismatch);
                }
                copy_verified(&mut zip, &part.name, &part.sha, handle, &mut chunk, cancel)?;
            }
            Body::Encode { elem } => {
                let bytes = read_blob(open_asset(assets, &part.sha, limits)?, &part.sha, cancel)?;
                let encoded = encode_blob(options.asset_codec, *elem, bytes, cancel)?;
                if encoded.codec != part.codec || encoded.stored.len() as u64 != part.stored_len {
                    return Err(PackageError::LengthMismatch);
                }
                zip.add(&part.name, &encoded.stored, cancel)?;
            }
        }
    }
    let stats = zip.finish(cancel)?;
    Ok(WriteReceipt {
        entries: stats.entries,
        bytes_written: stats.bytes_written,
        doc_revision: document.revision,
        zip64: stats.zip64,
    })
}
