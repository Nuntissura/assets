//! Health check of linked placed sources at open (lane SMB-PACKAGE-COMPLETENESS-2): a Folio
//! resolver decorator that answers `placed_link(uri, content_digest)` from the filesystem next to
//! the opened document and forwards every other port to the caller's resolver.
//!
//! The check is the STU-ASSET-007 resolution of a linked placement made by the host that opened
//! the file (the package format itself never reads outside its container): the relative `uri` is
//! resolved under `base` (the opened document's directory), traversal is refused, the file must be
//! a regular file inside `base` after symlink resolution, and its bytes are hashed in bounded
//! chunks. Available = exists and the hash matches; HashMismatch = exists, hash differs
//! (`modified`); Absent = no such file (`missing`); Unauthorized = the uri leaves `base` or the
//! file cannot be read; Unsupported = the recorded digest is not a sha256 (it cannot be checked)
//! or the file exceeds `max_bytes`.
#![deny(clippy::arithmetic_side_effects)]

use crate::hashing::{hex, is_hex64};
use hsk_studio_folio::{self as folio, Resolution, Resolver};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{ErrorKind, Read},
    path::{Component, Path, PathBuf},
};

/// Default bound on the size of a linked file hashed at open (4 GiB).
pub const MAX_LINKED_FILE_BYTES: u64 = 1 << 32;

/// Resolver decorator that checks linked placed sources under `base`.
pub struct LinkedFileResolver<'a> {
    inner: &'a dyn Resolver,
    base: PathBuf,
    max_bytes: u64,
}

impl<'a> LinkedFileResolver<'a> {
    /// `base` is the directory of the opened document.
    pub fn new(inner: &'a dyn Resolver, base: impl Into<PathBuf>) -> Self {
        Self { inner, base: base.into(), max_bytes: MAX_LINKED_FILE_BYTES }
    }

    /// The same with another size bound.
    pub fn with_max_bytes(mut self, max_bytes: u64) -> Self {
        self.max_bytes = max_bytes;
        self
    }

    /// The health of one linked file (see the module documentation).
    pub fn check(&self, uri: &str, content_digest: &str) -> Resolution {
        let wanted = content_digest
            .strip_prefix("sha256-")
            .or_else(|| content_digest.strip_prefix("sha256:"))
            .unwrap_or(content_digest);
        if !is_hex64(wanted) {
            return Resolution::Unsupported;
        }
        // Relative, forward-slash, no drive, no root, no `..`, no `.` games, no empty segment.
        if uri.is_empty()
            || uri.contains(['\\', ':', '\0'])
            || uri.starts_with('/')
            || uri.split('/').any(|seg| seg.is_empty() || seg == "." || seg == "..")
        {
            return Resolution::Unauthorized;
        }
        let relative = Path::new(uri);
        if !relative.components().all(|c| matches!(c, Component::Normal(_))) {
            return Resolution::Unauthorized;
        }
        let Ok(base) = self.base.canonicalize() else {
            return Resolution::Absent;
        };
        let target = match base.join(relative).canonicalize() {
            Ok(t) => t,
            Err(e) if e.kind() == ErrorKind::NotFound => return Resolution::Absent,
            Err(_) => return Resolution::Unauthorized,
        };
        if !target.starts_with(&base) {
            return Resolution::Unauthorized;
        }
        let Ok(meta) = target.metadata() else {
            return Resolution::Unauthorized;
        };
        if !meta.is_file() {
            return Resolution::Absent;
        }
        if meta.len() > self.max_bytes {
            return Resolution::Unsupported;
        }
        let Ok(mut file) = File::open(&target) else {
            return Resolution::Unauthorized;
        };
        let mut hasher = Sha256::new();
        let mut chunk = vec![0u8; 1 << 20];
        let mut total: u64 = 0;
        loop {
            let n = match file.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(_) => return Resolution::Unauthorized,
            };
            total = total.saturating_add(n as u64);
            if total > self.max_bytes {
                return Resolution::Unsupported;
            }
            hasher.update(&chunk[..n]);
        }
        if hex(&hasher.finalize()) == wanted {
            Resolution::Available
        } else {
            Resolution::HashMismatch
        }
    }
}

impl Resolver for LinkedFileResolver<'_> {
    fn primitive(&self, schema_id: &str, object_id: &str) -> Resolution {
        self.inner.primitive(schema_id, object_id)
    }
    fn profile(&self, profile_id: &str) -> Resolution {
        self.inner.profile(profile_id)
    }
    fn tile(&self, tile: &folio::TileRef) -> Resolution {
        self.inner.tile(tile)
    }
    fn format_supported(&self, format: &str) -> bool {
        self.inner.format_supported(format)
    }
    fn primitive_layer_binding(&self, kind: &str, schema_id: &str) -> bool {
        self.inner.primitive_layer_binding(kind, schema_id)
    }
    fn media(&self, id: &str, digest: &folio::ContentDigest) -> Resolution {
        self.inner.media(id, digest)
    }
    fn story_content(&self, id: &str, digest: &folio::ContentDigest) -> Resolution {
        self.inner.story_content(id, digest)
    }
    fn font(&self, family: &str, style: &str) -> Resolution {
        self.inner.font(family, style)
    }
    fn audio(&self, audio_id: &str) -> Resolution {
        self.inner.audio(audio_id)
    }
    fn lut(&self, lut: &str, content_digest: Option<&folio::ContentDigest>) -> Resolution {
        self.inner.lut(lut, content_digest)
    }
    fn placed_link(&self, uri: &str, content_digest: &str) -> Resolution {
        self.check(uri, content_digest)
    }
}
