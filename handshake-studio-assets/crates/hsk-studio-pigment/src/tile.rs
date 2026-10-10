//! Portable resolved buffers; no artifact store or persisted primitive.
use crate::Error;
use hsk_studio_folio::{GeometryUnit, TileRef};
use hsk_studio_prism::{ProfileInput, profile_hash};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rect {
    pub x: i64,
    pub y: i64,
    pub width: u32,
    pub height: u32,
}
impl Rect {
    pub fn end(self) -> Result<(i64, i64), Error> {
        Ok((
            self.x
                .checked_add(i64::from(self.width))
                .ok_or(Error::Overflow)?,
            self.y
                .checked_add(i64::from(self.height))
                .ok_or(Error::Overflow)?,
        ))
    }
    pub fn intersect(self, other: Self) -> Result<Option<Self>, Error> {
        let (a, b) = self.end()?;
        let (c, d) = other.end()?;
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let e = a.min(c);
        let f = b.min(d);
        if e <= x || f <= y {
            return Ok(None);
        }
        Ok(Some(Self {
            x,
            y,
            width: u32::try_from(e - x).map_err(|_| Error::Overflow)?,
            height: u32::try_from(f - y).map_err(|_| Error::Overflow)?,
        }))
    }
    pub fn area(self) -> u64 {
        u64::from(self.width) * u64::from(self.height)
    }
    pub fn contains(self, other: Self) -> Result<bool, Error> {
        let (a, b) = self.end()?;
        let (c, d) = other.end()?;
        Ok(self.x <= other.x && self.y <= other.y && a >= c && b >= d)
    }
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Grid {
    pub origin_x: i64,
    pub origin_y: i64,
    pub tile_width: u32,
    pub tile_height: u32,
}
impl Grid {
    pub fn bounds(self, column: i64, row: i64) -> Result<Rect, Error> {
        if self.tile_width == 0 || self.tile_height == 0 {
            return Err(Error::InvalidInput);
        }
        let x = i128::from(self.origin_x) + i128::from(column) * i128::from(self.tile_width);
        let y = i128::from(self.origin_y) + i128::from(row) * i128::from(self.tile_height);
        let r = Rect {
            x: i64::try_from(x).map_err(|_| Error::Overflow)?,
            y: i64::try_from(y).map_err(|_| Error::Overflow)?,
            width: self.tile_width,
            height: self.tile_height,
        };
        r.end()?;
        Ok(r)
    }
}
pub struct ResolvedTile<'a> {
    pub tile_ref: &'a TileRef,
    pub bounds: Rect,
    pub sample_format: &'a str,
    pub colour_bytes: &'a [u8],
    pub colour_stride_bytes: u64,
    pub colour_sha256: [u8; 32],
    pub alpha_bytes: &'a [u8],
    pub alpha_stride_bytes: u64,
    pub alpha_sha256: [u8; 32],
    pub transfer: &'a str,
    pub alpha_association: &'a str,
    pub profile: ProfileInput<'a>,
}
impl ResolvedTile<'_> {
    pub fn validate_shape(&self, grid: Grid) -> Result<(), Error> {
        if self.sample_format != "rgb_f32le"
            || self.transfer != "linear_light"
            || self.alpha_association != "straight"
        {
            return Err(Error::Unsupported);
        }
        if self.bounds != grid.bounds(self.tile_ref.column, self.tile_ref.row)? {
            return Err(Error::InvalidInput);
        }
        for (length, expected) in [
            (&self.tile_ref.width, self.bounds.width),
            (&self.tile_ref.height, self.bounds.height),
        ] {
            if length.unit != GeometryUnit::Px
                || !length.value.is_finite()
                || length.value != f64::from(expected)
            {
                return Err(Error::InvalidInput);
            }
        }
        if self.tile_ref.format != self.sample_format
            || self.tile_ref.colour_profile_id != self.profile.profile_id.as_str()
            || self.profile.profile_id.prefix() != "SCPF"
        {
            return Err(Error::InvalidInput);
        }
        if self.tile_ref.artifact_manifest_id.is_empty()
            || self.tile_ref.artifact_manifest_id.len() > 512
            || self.tile_ref.content_digest.algorithm.is_empty()
            || self.tile_ref.content_digest.algorithm.len() > 64
            || self.tile_ref.content_digest.digest.is_empty()
            || self.tile_ref.content_digest.digest.len() > 512
        {
            return Err(Error::InvalidInput);
        }
        plane(
            self.colour_bytes,
            self.colour_stride_bytes,
            self.bounds,
            12,
            4,
        )?;
        plane(self.alpha_bytes, self.alpha_stride_bytes, self.bounds, 4, 4)?;
        Ok(())
    }
    pub fn validate_hashes(&self) -> Result<(), Error> {
        if profile_hash(self.colour_bytes) != self.colour_sha256
            || profile_hash(self.alpha_bytes) != self.alpha_sha256
        {
            Err(Error::HashMismatch)
        } else {
            Ok(())
        }
    }
    pub fn bytes(&self) -> Result<u64, Error> {
        u64::try_from(self.colour_bytes.len())
            .map_err(|_| Error::Overflow)?
            .checked_add(u64::try_from(self.alpha_bytes.len()).map_err(|_| Error::Overflow)?)
            .ok_or(Error::Overflow)
    }
    pub(crate) fn pixel(&self, x: i64, y: i64) -> ([f32; 3], f32) {
        let row = (y - self.bounds.y) as usize;
        let col = (x - self.bounds.x) as usize;
        let at = row * self.colour_stride_bytes as usize + col * 12;
        let rgb = std::array::from_fn(|i| {
            f32::from_le_bytes(
                self.colour_bytes[at + i * 4..at + i * 4 + 4]
                    .try_into()
                    .expect("validated pixel"),
            )
        });
        let at = row * self.alpha_stride_bytes as usize + col * 4;
        (
            rgb,
            f32::from_le_bytes(
                self.alpha_bytes[at..at + 4]
                    .try_into()
                    .expect("validated pixel"),
            ),
        )
    }
}
fn plane(bytes: &[u8], stride: u64, bounds: Rect, bpp: u64, alignment: u64) -> Result<(), Error> {
    let row = u64::from(bounds.width)
        .checked_mul(bpp)
        .ok_or(Error::Overflow)?;
    let size = stride
        .checked_mul(u64::from(bounds.height))
        .ok_or(Error::Overflow)?;
    if stride < row
        || !stride.is_multiple_of(alignment)
        || usize::try_from(size).map_err(|_| Error::Overflow)? != bytes.len()
    {
        return Err(Error::MalformedStride);
    }
    Ok(())
}
pub struct Mask<'a> {
    pub bounds: Rect,
    pub sample_format: &'a str,
    pub bytes: &'a [u8],
    pub stride_bytes: u64,
    pub sha256: [u8; 32],
}
impl Mask<'_> {
    pub fn validate(&self, rectangle: Rect) -> Result<(), Error> {
        let size = match self.sample_format {
            "coverage_u8" => 1,
            "coverage_u16le" => 2,
            _ => return Err(Error::Unsupported),
        };
        if !self.bounds.contains(rectangle)? {
            return Err(Error::CoverageGap);
        }
        plane(self.bytes, self.stride_bytes, self.bounds, size, size)?;
        if profile_hash(self.bytes) != self.sha256 {
            return Err(Error::HashMismatch);
        }
        Ok(())
    }
    pub(crate) fn coverage(&self, x: i64, y: i64) -> f64 {
        let size = if self.sample_format == "coverage_u8" {
            1
        } else {
            2
        };
        let at = (y - self.bounds.y) as usize * self.stride_bytes as usize
            + (x - self.bounds.x) as usize * size;
        if size == 1 {
            f64::from(self.bytes[at]) / 255.0
        } else {
            f64::from(u16::from_le_bytes(
                self.bytes[at..at + 2].try_into().expect("validated mask"),
            )) / 65535.0
        }
    }
}
/// Immutable bytes whose every clone retains its counted output/preimage lease.
#[derive(Clone)]
pub struct OwnedPlane {
    bytes: Arc<[u8]>,
    lease: crate::Lease,
}
impl OwnedPlane {
    pub(crate) fn new(bytes: Arc<[u8]>, lease: crate::Lease) -> Self {
        Self { bytes, lease }
    }
    pub fn lease_handle(&self) -> u64 {
        self.lease.handle()
    }
}
impl std::ops::Deref for OwnedPlane {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        &self.bytes
    }
}
impl std::fmt::Debug for OwnedPlane {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OwnedPlane")
            .field("byte_len", &self.bytes.len())
            .field("lease_handle", &self.lease.handle())
            .finish()
    }
}
#[derive(Debug)]
pub struct Planes {
    pub tile_ref: TileRef,
    pub profile_sha256: [u8; 32],
    pub bounds: Rect,
    pub colour: OwnedPlane,
    pub alpha: OwnedPlane,
    pub colour_stride_bytes: u64,
    pub alpha_stride_bytes: u64,
    pub colour_sha256: [u8; 32],
    pub alpha_sha256: [u8; 32],
}
impl Planes {
    pub(crate) fn copied(t: &ResolvedTile<'_>, lease: crate::Lease) -> Self {
        Self {
            tile_ref: t.tile_ref.clone(),
            profile_sha256: t.profile.expected_sha256,
            bounds: t.bounds,
            colour: OwnedPlane::new(Arc::from(t.colour_bytes), lease.clone()),
            alpha: OwnedPlane::new(Arc::from(t.alpha_bytes), lease),
            colour_stride_bytes: t.colour_stride_bytes,
            alpha_stride_bytes: t.alpha_stride_bytes,
            colour_sha256: t.colour_sha256,
            alpha_sha256: t.alpha_sha256,
        }
    }
}
