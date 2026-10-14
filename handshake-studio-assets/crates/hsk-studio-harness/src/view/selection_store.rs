//! The active selection as a document entry (lane SMB-HARNESS-UI-3 slice 3): saved with the
//! package (session document entry `selection`, `studio/selection.<sha256>`) and restored on
//! reopen. The package has no selection record (Folio has no StudioSelectionSet; pigment owns the
//! coverage model), so the harness, which owns the viewer selection, owns this byte format:
//!
//! `HSKSEL01` (8 bytes) | x i64 | y i64 | width u32 | height u32 | runs u32 | runs x (length u32,
//! coverage u16), little-endian; runs cover the plane row-major exactly once.
//!
//! The bytes come back from a file, so decoding is hostile-input safe: exact length, dimensions
//! bounded by pigment's selection limits before anything is allocated, every run non-empty and the
//! running total checked against the plane size before it grows (tests in tests/viewer_selection.rs).
#![deny(clippy::arithmetic_side_effects)]
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::selection::{CoveragePlane, SelectionLimits};

const MAGIC: &[u8; 8] = b"HSKSEL01";
const HEADER: usize = 8 + 8 + 8 + 4 + 4 + 4;
const RUN: usize = 6;

/// Run-length encoding of the plane.
pub fn encode(plane: &CoveragePlane) -> Vec<u8> {
    let mut runs: Vec<(u32, u16)> = Vec::new();
    for v in &plane.data {
        match runs.last_mut() {
            Some((n, value)) if *value == *v && *n < u32::MAX => *n = n.saturating_add(1),
            _ => runs.push((1, *v)),
        }
    }
    let mut out = Vec::with_capacity(HEADER.saturating_add(runs.len().saturating_mul(RUN)));
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&plane.bounds.x.to_le_bytes());
    out.extend_from_slice(&plane.bounds.y.to_le_bytes());
    out.extend_from_slice(&plane.bounds.width.to_le_bytes());
    out.extend_from_slice(&plane.bounds.height.to_le_bytes());
    out.extend_from_slice(&u32::try_from(runs.len()).unwrap_or(u32::MAX).to_le_bytes());
    for (n, v) in runs {
        out.extend_from_slice(&n.to_le_bytes());
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}

fn take<const N: usize>(bytes: &[u8], at: &mut usize) -> Result<[u8; N], &'static str> {
    let end = at.checked_add(N).ok_or("selection_truncated")?;
    let slice = bytes.get(*at..end).ok_or("selection_truncated")?;
    *at = end;
    slice.try_into().map_err(|_| "selection_truncated")
}

/// Decodes [`encode`]'s bytes; typed refusal codes (`selection_*`).
pub fn decode(bytes: &[u8], limits: &SelectionLimits) -> Result<CoveragePlane, &'static str> {
    if bytes.get(..8) != Some(&MAGIC[..]) {
        return Err("selection_format");
    }
    let mut at = 8usize;
    let x = i64::from_le_bytes(take(bytes, &mut at)?);
    let y = i64::from_le_bytes(take(bytes, &mut at)?);
    let width = u32::from_le_bytes(take(bytes, &mut at)?);
    let height = u32::from_le_bytes(take(bytes, &mut at)?);
    let runs = u32::from_le_bytes(take(bytes, &mut at)?);
    let runs = usize::try_from(runs).map_err(|_| "selection_size")?;
    let expected = runs.checked_mul(RUN).and_then(|n| n.checked_add(HEADER)).ok_or("selection_size")?;
    if bytes.len() != expected {
        return Err("selection_length");
    }
    let bounds = Rect { x, y, width, height };
    bounds.end().map_err(|_| "selection_bounds")?;
    let area = u64::from(width).checked_mul(u64::from(height)).ok_or("selection_size")?;
    if area.checked_mul(2).is_none_or(|b| b > limits.max_bytes) {
        return Err("selection_over_budget");
    }
    let area = usize::try_from(area).map_err(|_| "selection_size")?;
    // Allocated only after the dimensions passed the limit.
    let mut plane = CoveragePlane::filled(bounds, 0, limits).map_err(|_| "selection_over_budget")?;
    let mut filled = 0usize;
    for _ in 0..runs {
        let n = usize::try_from(u32::from_le_bytes(take(bytes, &mut at)?)).map_err(|_| "selection_size")?;
        let v = u16::from_le_bytes(take(bytes, &mut at)?);
        let end = filled.checked_add(n).filter(|e| n > 0 && *e <= area).ok_or("selection_runs")?;
        plane.data.get_mut(filled..end).ok_or("selection_runs")?.fill(v);
        filled = end;
    }
    if filled != area {
        return Err("selection_runs");
    }
    Ok(plane)
}
