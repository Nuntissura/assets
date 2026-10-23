//! Native Adobe project formats (PRPROJ / AEP / SESX): explicit missing-decoder scope
//! (STU-IO-186). Detection uses magic bytes and the XML root element name only; no decoding and
//! no reverse engineering. Every detection string is a CANDIDATE until a black-box save of a
//! controlled project confirms it (UNVERIFIED). Editable import stays blocked, loudly.
use crate::error::{ErrorKind, InteropError};
use crate::model::{Disposition, LossNote};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeFormat {
    /// Premiere project candidate (gzip container).
    Prproj,
    /// After Effects binary project candidate (RIFX container).
    AepBinary,
    /// After Effects XML project candidate.
    Aepx,
    /// Audition session candidate (XML).
    Sesx,
    Unknown,
}

impl NativeFormat {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Prproj => "prproj",
            Self::AepBinary => "aep",
            Self::Aepx => "aepx",
            Self::Sesx => "sesx",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NativeSniff {
    pub format: NativeFormat,
    pub evidence: &'static str,
    pub len: u64,
}

/// First XML element name after the declaration, comments and doctype, within the first 4 KiB.
fn xml_root(bytes: &[u8]) -> Option<&[u8]> {
    let head = &bytes[..bytes.len().min(4096)];
    let mut i = 0;
    while i < head.len() {
        if head[i] == b'<' {
            match head.get(i + 1) {
                Some(b'?') | Some(b'!') => {}
                Some(c) if c.is_ascii_alphabetic() => {
                    let start = i + 1;
                    let end = head[start..]
                        .iter()
                        .position(|b| b.is_ascii_whitespace() || matches!(b, b'>' | b'/'))
                        .map_or(head.len(), |n| start + n);
                    return Some(&head[start..end]);
                }
                _ => return None,
            }
        }
        i += 1;
    }
    None
}

pub fn sniff_native(bytes: &[u8]) -> NativeSniff {
    let len = bytes.len() as u64;
    let found = |format, evidence| NativeSniff {
        format,
        evidence,
        len,
    };
    if bytes.len() < 4 {
        return found(NativeFormat::Unknown, "too_short");
    }
    if bytes.starts_with(&[0x1F, 0x8B]) {
        return found(NativeFormat::Prproj, "gzip_magic_candidate_unverified");
    }
    if bytes.starts_with(b"RIFX") {
        return found(NativeFormat::AepBinary, "rifx_magic_candidate_unverified");
    }
    let trimmed = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    if trimmed.first() == Some(&b'<') || trimmed.starts_with(b" ") || trimmed.starts_with(b"\n") {
        return match xml_root(trimmed) {
            Some(root) if root.eq_ignore_ascii_case(b"sesx") => {
                found(NativeFormat::Sesx, "xml_root_sesx_candidate_unverified")
            }
            Some(root) if root.eq_ignore_ascii_case(b"AfterEffectsProject") => found(
                NativeFormat::Aepx,
                "xml_root_aftereffectsproject_candidate_unverified",
            ),
            _ => found(NativeFormat::Unknown, "xml_root_unrecognized"),
        };
    }
    found(NativeFormat::Unknown, "no_known_magic")
}

/// Why a native import is blocked: the format guess, the exact length, and a `Missing` loss for
/// every entity class so a caller can keep the original bytes as an opaque blob (STU-IO-005).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeBlocked {
    pub format: NativeFormat,
    pub evidence: &'static str,
    pub len: u64,
    pub losses: Vec<LossNote>,
}

impl From<NativeBlocked> for InteropError {
    fn from(_: NativeBlocked) -> Self {
        InteropError::new(ErrorKind::NativeDecoderMissing)
    }
}

/// Native entity classes that an editable import would have to cover.
pub const NATIVE_ENTITY_CLASSES: [&str; 7] = [
    "project_items",
    "sequences",
    "tracks",
    "clips",
    "effects",
    "markers",
    "metadata",
];

/// Always blocked: there is no decoder. Never returns an editable result.
pub fn import_native(bytes: &[u8]) -> NativeBlocked {
    let sniff = sniff_native(bytes);
    let losses = NATIVE_ENTITY_CLASSES
        .iter()
        .map(|class| {
            LossNote::new(
                format!("native:{class}"),
                "editable_mapping",
                Disposition::Missing,
                "native_decoder_missing",
            )
        })
        .collect();
    NativeBlocked {
        format: sniff.format,
        evidence: sniff.evidence,
        len: sniff.len,
        losses,
    }
}
