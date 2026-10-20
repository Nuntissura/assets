//! CAD drawing detection (lane SMB-DXF-EPS-IMPORT): DXF (ASCII or binary) and DWG by header.
//! DWG is never decoded: no permissively licensed DWG reader exists (acadrust is MPL-2.0,
//! LibreDWG GPL-3.0, the ODA SDK proprietary; START_LOG lead decision 2026-10-16), so a DWG file
//! is refused with [`VectorError::DwgUnavailable`] naming the DXF export route, original bytes
//! preserved. DXF import is [`crate::import_dxf`] (feature `dxf`).
#![deny(clippy::arithmetic_side_effects)]
use crate::error::{PreservedBlob, Result, VectorError};
use crate::model::ImportPolicy;
use hsk_studio_accord::CancellationToken;

/// The supported route for a DWG drawing, carried by [`VectorError::DwgUnavailable`].
pub const DWG_ROUTE: &str = "export_dxf_from_the_cad_application_then_import_the_dxf";

/// Sentinel that opens a binary DXF file (22 bytes).
pub(crate) const BINARY_DXF_SENTINEL: &[u8] = b"AutoCAD Binary DXF\r\n\x1a\0";

/// Bytes inspected for the ASCII DXF sniff.
const SNIFF_WINDOW: usize = 4096;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CadKind {
    /// ASCII DXF: the first group pairs are `0`/`SECTION` (optionally after `999` comments).
    DxfAscii,
    /// Binary DXF (sentinel `AutoCAD Binary DXF`).
    DxfBinary,
    /// DWG: `AC` plus four version characters (for example `AC1032`); the version string when printable.
    Dwg { version: Option<String> },
    /// Neither.
    NotCad,
}

fn dwg_version(bytes: &[u8]) -> Option<Option<String>> {
    let head = bytes.get(..6)?;
    if !head.starts_with(b"AC") {
        return None;
    }
    let tail = head.get(2..)?;
    // AC1.2 / AC1.40 / AC1.50 / AC2.10 (pre-R10) and AC1001..AC1032: digits and dots only.
    if !tail.iter().all(|b| b.is_ascii_digit() || *b == b'.' || *b == 0) || !tail.first()?.is_ascii_digit() {
        return None;
    }
    let printable: String = head.iter().take_while(|b| **b != 0).map(|b| char::from(*b)).collect();
    Some(Some(printable))
}

/// First `(code, value)` pairs of an ASCII DXF, skipping `999` comments: `0` / `SECTION` opens it.
fn ascii_dxf(bytes: &[u8]) -> bool {
    let window = &bytes[..bytes.len().min(SNIFF_WINDOW)];
    let Ok(text) = std::str::from_utf8(window).or_else(|e| std::str::from_utf8(&window[..e.valid_up_to()])) else {
        return false;
    };
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut lines = text.lines().map(str::trim);
    for _ in 0..64 {
        let (Some(code), Some(value)) = (lines.next(), lines.next()) else { return false };
        match code {
            "999" => continue,
            "0" => return value.eq_ignore_ascii_case("SECTION"),
            _ => return false,
        }
    }
    false
}

/// Header sniff only; nothing is decoded.
pub fn identify_cad(bytes: &[u8]) -> CadKind {
    if bytes.starts_with(BINARY_DXF_SENTINEL) {
        return CadKind::DxfBinary;
    }
    if let Some(version) = dwg_version(bytes) {
        return CadKind::Dwg { version };
    }
    if ascii_dxf(bytes) {
        return CadKind::DxfAscii;
    }
    CadKind::NotCad
}

/// Always refuses: a DWG drawing is [`VectorError::DwgUnavailable`] (route [`DWG_ROUTE`], the
/// original bytes preserved); anything that is not DWG is `Unsupported("not_a_dwg_file")`.
pub fn import_dwg(bytes: &[u8], policy: &ImportPolicy, cancel: &CancellationToken) -> Result<()> {
    cancel.check().map_err(|_| VectorError::Canceled)?;
    if bytes.len() > policy.limits.max_bytes {
        return Err(VectorError::BudgetExceeded("bytes"));
    }
    match identify_cad(bytes) {
        CadKind::Dwg { version } => Err(VectorError::DwgUnavailable {
            version,
            route: DWG_ROUTE,
            preserved: PreservedBlob { kind: "dwg", bytes: bytes.to_vec() },
        }),
        _ => Err(VectorError::Unsupported("not_a_dwg_file")),
    }
}
