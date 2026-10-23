//! Adobe Illustrator (`.ai`) detection and the explicit native-editability blocker (STU-IO-186):
//! a PDF-compatible AI file parsed or previewed as PDF is NOT native editability, and a missing
//! decoder is a blocking scope gap, never a silent exclusion. The original bytes are preserved.
use crate::error::{PreservedBlob, Result, VectorError};
use crate::model::{ImportPolicy, VectorProposal};
use hsk_studio_accord::CancellationToken;

/// Bytes inspected for private-data markers.
const SNIFF_WINDOW: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AiKind {
    /// Neither PDF nor PostScript (for example SVG text).
    NotAi,
    /// `%PDF-` header. `has_private_data` is true when an Illustrator private-data marker is
    /// present in the first 64 KiB. UNVERIFIED: marker names have not been confirmed on an
    /// Illustrator-saved fixture; a PDF without them may still be an AI file.
    PdfCompatible { has_private_data: bool },
    /// `%!PS-Adobe` header: legacy AI (before PDF compatibility) or EPS.
    LegacyPostScript,
    /// Too short to decide.
    Unknown,
}

fn contains(window: &[u8], needle: &[u8]) -> bool {
    window.windows(needle.len()).any(|w| w == needle)
}

/// Header sniff only; nothing is decoded.
pub fn identify_ai(bytes: &[u8]) -> AiKind {
    if bytes.len() < 5 {
        return AiKind::Unknown;
    }
    if bytes.starts_with(b"%PDF-") {
        let window = &bytes[..bytes.len().min(SNIFF_WINDOW)];
        let has_private_data = contains(window, b"/Illustrator")
            || contains(window, b"AIPrivateData")
            || contains(window, b"%AI");
        return AiKind::PdfCompatible { has_private_data };
    }
    if bytes.starts_with(b"%!PS-Adobe") {
        return AiKind::LegacyPostScript;
    }
    AiKind::NotAi
}

/// Always refuses with [`VectorError::NativeEditabilityBlocked`] until a native decoder exists.
/// The original bytes travel in the error as a quarantined preservation blob.
pub fn import_ai(
    bytes: &[u8],
    policy: &ImportPolicy,
    cancel: &CancellationToken,
) -> Result<VectorProposal> {
    cancel.check().map_err(|_| VectorError::Canceled)?;
    if bytes.len() > policy.limits.max_bytes {
        return Err(VectorError::BudgetExceeded("bytes"));
    }
    let (kind, reason) = match identify_ai(bytes) {
        AiKind::PdfCompatible { .. } => (
            "ai_pdf_compatible",
            "native_ai_decoder_missing_pdf_preview_is_not_editability",
        ),
        AiKind::LegacyPostScript => ("ai_postscript", "native_ai_decoder_missing_postscript"),
        AiKind::NotAi | AiKind::Unknown => return Err(VectorError::Unsupported("not_an_ai_file")),
    };
    Err(VectorError::NativeEditabilityBlocked {
        version: None,
        reason,
        preserved: PreservedBlob {
            kind,
            bytes: bytes.to_vec(),
        },
    })
}
