//! Placed and smart-object layers (`SoLd` / `SoLE`, legacy `PlLd`) and the document's linked files
//! (`lnk2`, `lnk3`, `lnkD`, `lnkE`) mapped to Folio placed-asset records (lane SMB-PS-ADJUST-STYLES-2;
//! STU-RAS-012..014: a shared `StudioPlacedSource` plus a `StudioPlacedAsset` instance on the raster
//! layer, whose tiles stay the cached display pixels).
//!
//! Byte layouts: Adobe Photoshop File Formats Specification "Placed Layer Data" (`soLD` key, version,
//! version-16 descriptor) and "Linked Layer" (per item: u64 length, `liFD` / `liFE` / `liFA`, version
//! 1..=7, Pascal unique id, Unicode file name, file type, creator, u64 data length, open-descriptor
//! flag and optional version-16 descriptor, then for `liFD` the raw file bytes; items padded to 4).
//! Checked on the psd-tools corpus files `layers/smartobject-layer.psd` (`SoLd` version 4 with
//! `Idnt`, `Trnf` and `nonAffineTransform` lists of eight doubles, `warp` with `warpStyle` =
//! `warpNone`, `Sz  ` {`Wdth`, `Hght`}; one `lnk2` `liFD` version-7 item holding a PNG),
//! `placedLayer.psd` and `smart-object-slice.psd`; PhotoCraft's PSD reader is a behaviour reference
//! only (no code copied).
//!
//! The instance transform: `Trnf` holds the four corners of the placed source in document pixels
//! (top-left, top-right, bottom-right, bottom-left); an affine instance maps the source rect
//! `(0, 0, Wdth, Hght)` onto them, so `a = (x1 - x0) / W`, `b = (y1 - y0) / W`, `c = (x3 - x0) / H`,
//! `d = (y3 - y0) / H`, `e = x0`, `f = y0` (Folio's `[a, b, c, d, e, f]`). A quad that is not a
//! parallelogram, a `nonAffineTransform` different from `Trnf`, or a warp other than `warpNone` (or an
//! identity `warpCustom` mesh, [`identity_mesh`]) is a
//! typed loss: the layer keeps its cached pixels only. The source: an embedded file (`liFD`) becomes
//! an embedded source naming the Studio document the host mints for it ([`crate::IdMint`]
//! `placed_document_id`; none minted = typed loss) at the SHA-256 of the embedded bytes; an
//! external file (`liFE`) a linked source at its file name with health `missing` (the importer
//! reads no file outside the PSD); an alias (`liFA`) is a typed loss. Every count and length is
//! bounded before it is used (V4-01 / V4-02).
#![deny(clippy::arithmetic_side_effects)]

use crate::adjust::{Mapped, Reason};
use crate::descriptor;
use crate::layers::{PsdLayer, TaggedBlock};
use crate::limits::Limits;
use crate::loss::LossKind;
use crate::reader::Reader;

/// Linked-file items read from one document (a hostile count is refused beyond this).
pub(crate) const MAX_LINKED_FILES: usize = 4096;
/// Tolerance of the parallelogram check, relative to the quad size.
const AFFINE_TOLERANCE: f64 = 1e-6;

/// What a linked-file item holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum LinkedData {
    /// `liFD`: the file is inside the PSD; SHA-256 of its bytes.
    Embedded([u8; 32]),
    /// `liFE`: an external file.
    External,
    /// `liFA`: an alias record.
    Alias,
}

/// One linked-file item.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Linked {
    pub unique_id: String,
    pub file_name: String,
    pub data: LinkedData,
}

fn trunc<T>(r: crate::error::Result<T>) -> std::result::Result<T, Reason> {
    r.map_err(|_| "linked_file_truncated")
}

/// A Unicode string: u32 code units then UTF-16BE (a trailing NUL dropped).
fn unicode(r: &mut Reader<'_>) -> std::result::Result<String, Reason> {
    let units = usize::try_from(trunc(r.u32("lnk"))?).map_err(|_| "linked_file_truncated")?;
    let bytes = trunc(r.bytes(units.checked_mul(2).ok_or("linked_file_truncated")?, "lnk"))?;
    let words: Vec<u16> = bytes.chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
    let mut s = String::from_utf16_lossy(&words);
    if s.ends_with('\0') {
        s.pop();
    }
    Ok(s)
}

/// The linked-file items of the document's global blocks, and a reason for every block refused.
pub(crate) fn linked_files(blocks: &[TaggedBlock], limits: &Limits) -> (Vec<Linked>, Vec<Reason>) {
    let mut out = Vec::new();
    let mut refused = Vec::new();
    for b in blocks.iter().filter(|b| matches!(&b.key, b"lnk2" | b"lnk3" | b"lnkD" | b"lnkE")) {
        if let Err(reason) = items(&b.data, limits, &mut out) {
            refused.push(reason);
        }
    }
    (out, refused)
}

fn items(data: &[u8], limits: &Limits, out: &mut Vec<Linked>) -> std::result::Result<(), Reason> {
    let mut r = Reader::new(data);
    while r.remaining() >= 8 {
        let len = usize::try_from(trunc(r.u64("lnk"))?).map_err(|_| "linked_file_truncated")?;
        let item = trunc(r.bytes(len, "lnk"))?;
        if out.len() >= MAX_LINKED_FILES {
            return Err("linked_file_count");
        }
        out.push(linked(item, limits)?);
        // Items are padded to a multiple of 4.
        let pad = len.checked_rem(4).map_or(0, |m| if m == 0 { 0 } else { 4usize.saturating_sub(m) });
        r.skip_up_to(pad);
    }
    Ok(())
}

fn linked(item: &[u8], limits: &Limits) -> std::result::Result<Linked, Reason> {
    let mut r = Reader::new(item);
    let kind = trunc(r.array4("lnk"))?;
    let version = trunc(r.u32("lnk"))?;
    if !(1..=7).contains(&version) {
        return Err("linked_file_version_unsupported");
    }
    let id_len = usize::from(trunc(r.u8("lnk"))?);
    let id = trunc(r.bytes(id_len, "lnk"))?;
    let unique_id: String = String::from_utf8_lossy(id).into_owned();
    let file_name = unicode(&mut r)?;
    trunc(r.bytes(8, "lnk"))?; // file type, creator
    let data_len = usize::try_from(trunc(r.u64("lnk"))?).map_err(|_| "linked_file_truncated")?;
    let open = trunc(r.u8("lnk"))? != 0;
    let rest = if open {
        descriptor::parse_versioned_rest(r.rest(), &[16], limits)
            .map_err(|_| "linked_file_descriptor_unreadable")?
            .1
    } else {
        r.rest()
    };
    let data = match &kind {
        b"liFD" => {
            let bytes = rest.get(..data_len).ok_or("linked_file_truncated")?;
            LinkedData::Embedded(hsk_studio_prism::profile_hash(bytes))
        }
        b"liFE" => LinkedData::External,
        b"liFA" => LinkedData::Alias,
        _ => return Err("linked_file_kind_unknown"),
    };
    Ok(Linked { unique_id, file_name, data })
}

/// A placed instance of one layer: the source's unique id and size, and the affine instance
/// transform.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Placed {
    pub unique_id: String,
    pub width: f64,
    pub height: f64,
    pub transform: [f64; 6],
}

fn eight(v: Option<&descriptor::Value>) -> Option<[f64; 8]> {
    let list = v?.list()?;
    if list.len() != 8 {
        return None;
    }
    let mut out = [0.0f64; 8];
    for (slot, x) in out.iter_mut().zip(list) {
        *slot = x.finite_number()?;
    }
    Some(out)
}

/// Whether a `warpCustom` warp is the identity: zero `warpValue` / `warpPerspective` /
/// `warpPerspectiveOther`, and a `customEnvelopeWarp.meshPoints` grid of `uOrder` x `vOrder` points
/// (`Hrzn` and `Vrtc` columns, row-major) evenly spaced over `bounds` (`Top`, `Left`, `Btom`, `Rght`).
/// Read from the psd-tools corpus file `placedLayer.psd`, whose four placed layers save an untouched
/// 4 x 4 mesh this way; any other mesh is a warp this importer does not map.
fn identity_mesh(w: &descriptor::Object) -> bool {
    let num = |o: &descriptor::Object, k: &str| o.get(k).and_then(descriptor::Value::finite_number);
    if ["warpValue", "warpPerspective", "warpPerspectiveOther"]
        .iter()
        .any(|k| num(w, k).is_some_and(|v| v != 0.0))
    {
        return false;
    }
    let Some(b) = w.get("bounds").and_then(descriptor::Value::object) else {
        return false;
    };
    let (Some(top), Some(left), Some(bottom), Some(right)) = (num(b, "Top "), num(b, "Left"), num(b, "Btom"), num(b, "Rght"))
    else {
        return false;
    };
    let order = |k: &str| num(w, k).filter(|v| (2.0..=64.0).contains(v) && v.fract() == 0.0).map(|v| v as usize);
    let (Some(u), Some(v)) = (order("uOrder"), order("vOrder")) else {
        return false;
    };
    let Some(mesh) = w
        .get("customEnvelopeWarp")
        .and_then(descriptor::Value::object)
        .and_then(|c| c.get("meshPoints"))
        .and_then(descriptor::Value::object)
    else {
        return false;
    };
    let column = |k: &str| -> Option<Vec<f64>> {
        mesh.get(k)?.list()?.iter().map(descriptor::Value::finite_number).collect()
    };
    let (Some(h), Some(vt)) = (column("Hrzn"), column("Vrtc")) else {
        return false;
    };
    let Some(n) = u.checked_mul(v) else {
        return false;
    };
    if h.len() != n || vt.len() != n {
        return false;
    }
    let tol = AFFINE_TOLERANCE * (1.0 + (right - left).abs() + (bottom - top).abs());
    let (du, dv) = ((right - left) / (u as f64 - 1.0), (bottom - top) / (v as f64 - 1.0));
    h.chunks_exact(u).zip(vt.chunks_exact(u)).enumerate().all(|(row, (xs, ys))| {
        xs.iter().zip(ys).enumerate().all(|(col, (x, y))| {
            (x - (left + col as f64 * du)).abs() <= tol && (y - (top + row as f64 * dv)).abs() <= tol
        })
    })
}

/// Maps the placed-layer data of `layer`; `None` when it has none.
pub(crate) fn placed(layer: &PsdLayer, limits: &Limits) -> Option<std::result::Result<Mapped<Placed>, Reason>> {
    if let Some(b) = layer.block(b"SoLd").or_else(|| layer.block(b"SoLE")) {
        return Some(placed_data(&b.data, limits));
    }
    // A legacy `PlLd` alone carries no source size: the layer keeps its cached pixels.
    layer.block(b"PlLd").map(|_| Err("placed_layer_legacy_unmapped"))
}

fn placed_data(data: &[u8], limits: &Limits) -> std::result::Result<Mapped<Placed>, Reason> {
    let mut r = Reader::new(data);
    if trunc(r.array4("SoLd"))? != *b"soLD" {
        return Err("placed_layer_data_kind");
    }
    let version = trunc(r.u32("SoLd"))?;
    if !(4..=5).contains(&version) {
        return Err("placed_layer_data_version_unsupported");
    }
    let o = descriptor::parse_versioned(r.rest(), &[16], limits).map_err(|_| "placed_layer_descriptor_unreadable")?;
    let unique_id = match o.get("Idnt") {
        Some(descriptor::Value::Text(t)) if !t.is_empty() => t.clone(),
        _ => return Err("placed_layer_identity_missing"),
    };
    let size = o.get("Sz  ").and_then(descriptor::Value::object).ok_or("placed_layer_size_missing")?;
    let dim = |k: &str| size.get(k).and_then(descriptor::Value::finite_number).filter(|v| *v > 0.0);
    let (Some(width), Some(height)) = (dim("Wdth"), dim("Hght")) else {
        return Err("placed_layer_size_missing");
    };
    let quad = eight(o.get("Trnf")).ok_or("placed_layer_transform_missing")?;
    if let Some(v) = o.get("nonAffineTransform") {
        let non_affine = eight(Some(v)).ok_or("placed_layer_transform_missing")?;
        if non_affine.iter().zip(&quad).any(|(a, b)| (a - b).abs() > AFFINE_TOLERANCE * (1.0 + b.abs())) {
            return Err("placed_transform_non_affine");
        }
    }
    if let Some(w) = o.get("warp").and_then(descriptor::Value::object) {
        match w.get("warpStyle").and_then(descriptor::Value::enum_value) {
            None | Some("warpNone") => {}
            Some("warpCustom") if identity_mesh(w) => {}
            Some(_) => return Err("placed_warp_unmapped"),
        }
    }
    let [x0, y0, x1, y1, x2, y2, x3, y3] = quad;
    let span = (x1 - x0).abs() + (y1 - y0).abs() + (x3 - x0).abs() + (y3 - y0).abs() + 1.0;
    // A parallelogram: the bottom-right corner is top-right + bottom-left - top-left.
    if ((x1 + x3 - x0) - x2).abs() > AFFINE_TOLERANCE * span || ((y1 + y3 - y0) - y2).abs() > AFFINE_TOLERANCE * span {
        return Err("placed_transform_non_affine");
    }
    let transform = [(x1 - x0) / width, (y1 - y0) / width, (x3 - x0) / height, (y3 - y0) / height, x0, y0];
    let det = transform[0] * transform[3] - transform[1] * transform[2];
    if !transform.iter().all(|v| v.is_finite()) || det.is_nan() || det.abs() <= f64::EPSILON {
        return Err("placed_transform_singular");
    }
    Ok(Mapped {
        value: Placed { unique_id, width, height, transform },
        notes: vec![(LossKind::Preserved, "placed_asset_mapped_to_record")],
    })
}

/// A Folio key (`[A-Za-z0-9_-]`, at most 128 bytes) for the source of `unique_id`.
pub(crate) fn source_id(unique_id: &str) -> String {
    let clean: String = unique_id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .take(100)
        .collect();
    format!("psd-so-{clean}")
}

/// A relative URI for an external file name: the name without any directory, or `None` when
/// nothing usable remains.
pub(crate) fn link_uri(file_name: &str) -> Option<String> {
    let base = file_name.rsplit(['/', '\\']).next().unwrap_or("");
    let clean: String = base.chars().filter(|c| !c.is_control() && *c != ':').take(120).collect();
    (!clean.is_empty() && !clean.starts_with('.')).then_some(clean)
}
