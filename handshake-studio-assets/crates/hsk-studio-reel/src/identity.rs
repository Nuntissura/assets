//! Sampled source identity and relink (content-aware, never path-keyed).
//!
//! A source is identified by its length plus SHA-256 over deterministic windows (head, tail, evenly
//! spaced interior). That is a CLAIM of equivalence with sampled confidence, not proof: two files that
//! differ only between windows share a sampled identity. `relink` therefore reports a confidence
//! (`Full` only when the whole file was hashed or a full digest verified), reports ambiguity instead of
//! picking, and can verify a sampled match with the full-content SHA-256.
//!
//! Offsets, window spans and counters use checked or saturating arithmetic (V4-01).
#![deny(clippy::arithmetic_side_effects)]
use crate::error::{ReelError, check_cancel};
use crate::model::ReadSeek;
use hsk_studio_accord::CancellationToken;
use sha2::{Digest, Sha256};
use std::io::SeekFrom;

const DOMAIN: &[u8] = b"hsk-studio-reel/source-identity/v1";
const MAX_WINDOW: u32 = 1 << 20;
const MAX_WINDOWS: u32 = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IdentityParams {
    /// Bytes hashed per window (1..=1 MiB).
    pub window: u32,
    /// Number of windows: head, tail and `windows - 2` evenly spaced interior ones (2..=64).
    pub windows: u32,
}

impl Default for IdentityParams {
    fn default() -> Self {
        Self {
            window: 64 * 1024,
            windows: 8,
        }
    }
}

impl IdentityParams {
    fn validate(self) -> Result<Self, ReelError> {
        if self.window == 0
            || self.window > MAX_WINDOW
            || self.windows < 2
            || self.windows > MAX_WINDOWS
        {
            return Err(ReelError::LimitExceeded {
                what: "identity-params",
            });
        }
        Ok(self)
    }
    fn covered_bytes(self) -> u64 {
        // window <= 2^20 and windows <= 64 after `validate`, so the product cannot saturate.
        u64::from(self.window).saturating_mul(u64::from(self.windows))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceIdentity {
    pub len: u64,
    pub params: IdentityParams,
    pub digest: [u8; 32],
    /// The source was short enough that every byte was hashed (sampled identity == full identity).
    pub exhaustive: bool,
}

pub fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len().saturating_mul(2)), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

impl SourceIdentity {
    pub fn digest_hex(&self) -> String {
        hex(&self.digest)
    }
}

fn read_window(src: &mut dyn ReadSeek, offset: u64, buf: &mut [u8]) -> Result<(), ReelError> {
    src.seek(SeekFrom::Start(offset))?;
    src.read_exact(buf).map_err(|e| {
        if e.kind() == std::io::ErrorKind::UnexpectedEof {
            // The source shrank while it was being identified: its length claim no longer holds.
            ReelError::StaleGrant
        } else {
            ReelError::Io(e.kind())
        }
    })
}

/// Identity of `src`: length, parameters and SHA-256 over the framed windows. Reads at most
/// `window * windows` bytes with one `window`-sized buffer.
pub fn sampled_identity(
    src: &mut dyn ReadSeek,
    params: IdentityParams,
    cancel: &CancellationToken,
) -> Result<SourceIdentity, ReelError> {
    let params = params.validate()?;
    let len = src.seek(SeekFrom::End(0))?;
    let mut hash = Sha256::new();
    hash.update(DOMAIN);
    hash.update(len.to_le_bytes());
    hash.update(params.window.to_le_bytes());
    hash.update(params.windows.to_le_bytes());
    let window = u64::from(params.window);
    let window_len = usize::try_from(params.window).map_err(|_| ReelError::LimitExceeded {
        what: "identity-params",
    })?;
    let mut buf = vec![0u8; window_len];
    let exhaustive = len <= params.covered_bytes();
    let offset_error = ReelError::LimitExceeded {
        what: "identity-offset",
    };
    if exhaustive {
        let mut offset = 0u64;
        while offset < len {
            check_cancel(cancel)?;
            let n = window.min(len.saturating_sub(offset));
            let n_len = usize::try_from(n).map_err(|_| offset_error.clone())?;
            read_window(src, offset, &mut buf[..n_len])?;
            hash.update(&buf[..n_len]);
            offset = offset.checked_add(n).ok_or_else(|| offset_error.clone())?;
        }
    } else {
        // `validate` keeps windows >= 2 and not exhaustive means len > windows * window, so both the
        // span and the divisor are positive.
        let span = u128::from(len.saturating_sub(window));
        let divisor = u128::from(params.windows.saturating_sub(1));
        for i in 0..params.windows {
            check_cancel(cancel)?;
            let offset = span
                .checked_mul(u128::from(i))
                .and_then(|scaled| scaled.checked_div(divisor))
                .and_then(|o| u64::try_from(o).ok())
                .ok_or_else(|| offset_error.clone())?;
            read_window(src, offset, &mut buf)?;
            hash.update(&buf);
        }
    }
    Ok(SourceIdentity {
        len,
        params,
        digest: hash.finalize().into(),
        exhaustive,
    })
}

/// SHA-256 of the entire content (no framing: comparable with any external content hash).
pub fn full_digest(
    src: &mut dyn ReadSeek,
    chunk_bytes: usize,
    cancel: &CancellationToken,
) -> Result<[u8; 32], ReelError> {
    if chunk_bytes == 0 || chunk_bytes > (8 << 20) {
        return Err(ReelError::LimitExceeded {
            what: "digest-chunk-bytes",
        });
    }
    src.seek(SeekFrom::Start(0))?;
    let mut hash = Sha256::new();
    let mut buf = vec![0u8; chunk_bytes];
    loop {
        check_cancel(cancel)?;
        let n = src.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
    }
    Ok(hash.finalize().into())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelinkConfidence {
    /// Whole content matched (exhaustive sampling or a verified full digest).
    Full,
    /// Length and sampled windows matched; bytes between windows were not compared.
    Sampled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RelinkMatch {
    pub candidate: usize,
    pub confidence: RelinkConfidence,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelinkOutcome {
    Unique(RelinkMatch),
    /// More than one candidate matched: the caller (or the Operator) decides, reel never picks.
    Ambiguous(Vec<RelinkMatch>),
    NotFound {
        examined: usize,
        length_mismatch: usize,
        content_mismatch: usize,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RelinkQuery {
    pub expected: SourceIdentity,
    /// SHA-256 of the whole original content, when it was recorded.
    pub expected_full: Option<[u8; 32]>,
    /// Confirm every sampled match with the full digest (requires `expected_full`).
    pub verify_full: bool,
}

/// Find the candidates that are the same content as `query.expected`. Candidates are caller-provided
/// readers (reel never walks a file system); a length mismatch is rejected without reading content.
pub fn relink(
    query: &RelinkQuery,
    candidates: &mut [&mut dyn ReadSeek],
    chunk_bytes: usize,
    cancel: &CancellationToken,
) -> Result<RelinkOutcome, ReelError> {
    if query.verify_full && query.expected_full.is_none() {
        return Err(ReelError::Unsupported {
            what: "relink-verify-needs-full-digest",
        });
    }
    let (mut length_mismatch, mut content_mismatch) = (0usize, 0usize);
    let mut matches = Vec::new();
    for (candidate, src) in candidates.iter_mut().enumerate() {
        check_cancel(cancel)?;
        let len = src.seek(SeekFrom::End(0))?;
        if len != query.expected.len {
            length_mismatch = length_mismatch.saturating_add(1);
            continue;
        }
        let identity = sampled_identity(&mut **src, query.expected.params, cancel)?;
        if identity.digest != query.expected.digest {
            content_mismatch = content_mismatch.saturating_add(1);
            continue;
        }
        let mut confidence = if identity.exhaustive {
            RelinkConfidence::Full
        } else {
            RelinkConfidence::Sampled
        };
        if query.verify_full {
            if full_digest(&mut **src, chunk_bytes, cancel)?
                != query.expected_full.unwrap_or([0; 32])
            {
                content_mismatch = content_mismatch.saturating_add(1);
                continue;
            }
            confidence = RelinkConfidence::Full;
        }
        matches.push(RelinkMatch {
            candidate,
            confidence,
        });
    }
    Ok(match matches.len() {
        0 => RelinkOutcome::NotFound {
            examined: candidates.len(),
            length_mismatch,
            content_mismatch,
        },
        1 => RelinkOutcome::Unique(matches[0]),
        _ => RelinkOutcome::Ambiguous(matches),
    })
}
