//! Black and white level helpers for the proprietary container decoders.
//!
//! `plateau_white` is adapted from LightCraft `crates/raw/src/vendor/mod.rs` `white_from_data`
//! (https://github.com/storytold/lightcraft at af4c5e102c9f97529f582003b99c21b89067e906; MIT OR
//! Apache-2.0; attribution in this crate's `NOTICE`): the saturation level is the lower edge of a
//! spike at the top of the histogram (many more samples in the top band than in the band below it),
//! else the nominal full scale. `site_means` measures a per-CFA-site black level over an optically
//! masked region (LightCraft `black_from_columns`, same source).
// V4-01: this module reads decoded samples; every integer operation is checked or saturating.
#![deny(clippy::arithmetic_side_effects)]

/// A rectangle of the stored frame: columns `x0..x1`, rows `y0..y1`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Region {
    pub x0: u32,
    pub y0: u32,
    pub x1: u32,
    pub y1: u32,
}

impl Region {
    pub fn whole(width: u32, height: u32) -> Self {
        Self { x0: 0, y0: 0, x1: width, y1: height }
    }

    fn valid(&self, width: u32, height: u32) -> bool {
        self.x0 < self.x1 && self.y0 < self.y1 && self.x1 <= width && self.y1 <= height
    }
}

/// Mean of each site of a `rows x cols` repeat (anchored at the frame origin) over `region`;
/// `None` when the region is invalid or a site has no sample.
pub(crate) fn site_means(samples: &[u16], width: u32, height: u32, region: Region, repeat: (u32, u32)) -> Option<Vec<f64>> {
    let (rr, rc) = repeat;
    if !region.valid(width, height) || rr == 0 || rc == 0 {
        return None;
    }
    let sites = usize::try_from(rr.checked_mul(rc)?).ok()?;
    let (mut sum, mut n) = (vec![0f64; sites], vec![0u64; sites]);
    for y in region.y0..region.y1 {
        let row = usize::try_from(y).ok()?.checked_mul(usize::try_from(width).ok()?)?;
        for x in region.x0..region.x1 {
            let v = *samples.get(row.checked_add(usize::try_from(x).ok()?)?)?;
            let site = usize::try_from(y.checked_rem(rr)?.checked_mul(rc)?.checked_add(x.checked_rem(rc)?)?).ok()?;
            *sum.get_mut(site)? += f64::from(v);
            *n.get_mut(site)? = n.get(site)?.saturating_add(1);
        }
    }
    sum.iter().zip(&n).map(|(s, c)| (*c > 0).then(|| s / *c as f64)).collect()
}

/// Saturation plateau over `region` (LightCraft's rule): `Some(level)` when the top band of the
/// histogram is a spike, `None` when no plateau is present (the caller then uses the nominal
/// full scale of `full`).
pub(crate) fn plateau_white(samples: &[u16], width: u32, height: u32, region: Region, full: u32) -> Option<u32> {
    if !region.valid(width, height) {
        return None;
    }
    let mut hist = vec![0u64; usize::try_from(full).ok()?.checked_add(1)?];
    let mut total = 0u64;
    for y in region.y0..region.y1 {
        let row = usize::try_from(y).ok()?.checked_mul(usize::try_from(width).ok()?)?;
        let start = row.checked_add(usize::try_from(region.x0).ok()?)?;
        let end = row.checked_add(usize::try_from(region.x1).ok()?)?;
        for &v in samples.get(start..end)? {
            // samples above the nominal range are padding, not saturation
            if let Some(h) = hist.get_mut(usize::from(v)) {
                *h = h.saturating_add(1);
                total = total.saturating_add(1);
            }
        }
    }
    let min_plateau = (total / 20_000).max(16);
    let strays = min_plateau / 2;
    let mut above = 0u64;
    let mx = hist.iter().enumerate().rev().find_map(|(v, &n)| {
        above = above.saturating_add(n);
        (above > strays).then_some(v)
    })?;
    let mx = u32::try_from(mx).ok()?;
    let band = (mx / 256).max(1);
    let near = mx.saturating_sub(band);
    let below = near.saturating_sub(band);
    let count = |from: u32, to: u32| -> u64 {
        hist.get(from as usize..to as usize).map_or(0, |h| h.iter().fold(0u64, |a, b| a.saturating_add(*b)))
    };
    let top = count(near, full.saturating_add(1));
    let under = count(below, near);
    // A frame (almost) entirely at one level is not evidence of saturation: the spike must be a
    // minority of the region (at most 90 %).
    let minority = top.saturating_mul(10) <= total.saturating_mul(9);
    (mx > full / 2 && top >= min_plateau && top >= under.saturating_mul(4).saturating_add(4) && minority).then_some(near)
}

/// The white level the samples prove: the saturation plateau (`plateau_white`), else `ceiling`
/// itself when at least 16 samples of `region` reach it (highlights clipped at the code ceiling,
/// e.g. the full scale or a lossy curve's top). `None` when the sample holds no saturated region,
/// so the level stays unread.
pub(crate) fn measured_white(samples: &[u16], width: u32, height: u32, region: Region, ceiling: u32) -> Option<u32> {
    if let Some(w) = plateau_white(samples, width, height, region, ceiling) {
        return Some(w);
    }
    if !region.valid(width, height) {
        return None;
    }
    let mut at = 0u64;
    for y in region.y0..region.y1 {
        let row = usize::try_from(y).ok()?.checked_mul(usize::try_from(width).ok()?)?;
        let start = row.checked_add(usize::try_from(region.x0).ok()?)?;
        let end = row.checked_add(usize::try_from(region.x1).ok()?)?;
        at = at.saturating_add(samples.get(start..end)?.iter().filter(|v| u32::from(**v) >= ceiling).count() as u64);
    }
    (at >= 16).then_some(ceiling)
}

/// `unapplied` without the names in `applied`.
pub(crate) fn remaining(unapplied: &[&'static str], applied: &[&str]) -> Vec<&'static str> {
    unapplied.iter().copied().filter(|n| !applied.contains(n)).collect()
}

/// Canon maker note `SensorInfo` (0x00E0, ExifTool Canon tag names: word 1 sensor width, 2 sensor
/// height, 5..8 left/top/right/bottom border, inclusive) as an image region of a `width x height`
/// frame. The maker note is reached through the Exif IFD (0x8769) tag 0x927C and is an IFD whose
/// offsets are relative to the TIFF start. `None` when absent or not matching the frame.
pub(crate) fn canon_sensor_area(tiff: &crate::tiffep::Tiff<'_>, root: &crate::tiffep::Ifd, width: u32, height: u32) -> Option<Region> {
    let exif = tiff.ifd(crate::tiffep::first_u32(tiff, root.find(0x8769)?).ok()?).ok()?;
    let note = exif.find(0x927c)?;
    let mn = tiff.ifd(u32::try_from(note.value_offset).ok()?).ok()?;
    let info = tiff.u32s(mn.find(0x00e0)?).ok()?;
    let (&w, &h) = (info.get(1)?, info.get(2)?);
    let [left, top, right, bottom] = [*info.get(5)?, *info.get(6)?, *info.get(7)?, *info.get(8)?];
    if w != width || h != height || right >= width || bottom >= height || left >= right || top >= bottom {
        return None;
    }
    Some(Region { x0: left, y0: top, x1: right.checked_add(1)?, y1: bottom.checked_add(1)? })
}
