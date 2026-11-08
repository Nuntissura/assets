//! The layout PDF's asset resolver (lane SMB-LAYOUT-PLACE-1; [STU-LAY-028], [STU-PDF-002]): the tiles of every
//! graphic frame's raster layer as interop-layout's `PlacedRaster`, and the working profile's ICC bytes. egui-free.
//!
//! - `raster(layer_id)`: a graphic frame's layer only. Its tiles are 256-pixel cells from (0, 0) (`view::place`,
//!   "Layout pages"), each body the `rgb_f32le` colour plane then the `f32le` alpha plane, read from the viewer's
//!   tile map, else from the session's tile store (a reopened package's bodies are both); presented as the brush
//!   engine presents them (linear light, straight alpha, the tile's own profile id with Prism's linear working
//!   profile, `view::brush_mix::tile_view`). A layer one of whose bodies is absent or not one cell long resolves to
//!   nothing, so the engine reports `raster_unresolved` for it rather than drawing part of it.
//! - `profile(id)`: Prism's linear working profile for the document's RGB working space id, nothing else.
//! - `path`: nothing (rectangle and ellipse frames only; a path frame is refused at place).
#![deny(clippy::arithmetic_side_effects)]

use super::edit::{ALPHA_CELL, CELL, COLOUR_CELL};
use super::render::Assets;
use hsk_studio_accord::DomainId;
use hsk_studio_folio::{Payload, ProfileBinding, StudioDocument, StudioGraphicFrame, TileRef};
use hsk_studio_interop_layout::{PdfAssets, PlacedRaster};
use hsk_studio_nib::OwnedPath;
use hsk_studio_pigment::{Grid, Rect, ResolvedTile};
use hsk_studio_prism::{ProfileInput, WORKING_PROFILE_LINEAR_SRGB, profile_hash};
use std::collections::{BTreeMap, BTreeSet};

/// One resolved tile: its reference, its profile id and its body (colour then alpha plane).
struct Tile<'a> {
    tile: &'a TileRef,
    profile: DomainId,
    body: &'a [u8],
}

/// The resolver (module docs).
pub(super) struct LayoutAssets<'a> {
    layers: BTreeMap<&'a str, Vec<Tile<'a>>>,
    working: Option<&'a str>,
    profile_sha256: [u8; 32],
}

/// The tile bodies `frames`' layers reference in `doc`, by sha256 hex.
pub(super) fn needed(doc: &StudioDocument, frames: &[StudioGraphicFrame]) -> BTreeSet<String> {
    let framed: BTreeSet<&str> = frames.iter().map(|g| g.layer_id.as_str()).collect();
    doc.layers
        .iter()
        .filter(|l| framed.contains(l.layer_id.as_str()))
        .flat_map(|l| match &l.payload {
            Payload::Raster { tiles } => tiles.iter().map(|t| t.content_digest.digest.clone()).collect(),
            _ => Vec::new(),
        })
        .collect()
}

impl<'a> LayoutAssets<'a> {
    /// The resolver of `frames`' layers in `doc`, bodies from `bodies` in order (module docs).
    pub(super) fn new(doc: &'a StudioDocument, frames: &[StudioGraphicFrame], bodies: &[&'a Assets]) -> Self {
        let framed: BTreeSet<&str> = frames.iter().map(|g| g.layer_id.as_str()).collect();
        let cell = COLOUR_CELL.saturating_add(ALPHA_CELL);
        let mut layers = BTreeMap::new();
        for l in doc.layers.iter().filter(|l| framed.contains(l.layer_id.as_str())) {
            let Payload::Raster { tiles } = &l.payload else { continue };
            let resolved: Option<Vec<Tile<'a>>> = tiles
                .iter()
                .map(|t| {
                    let body = bodies.iter().find_map(|b| b.get(&t.content_digest.digest)).filter(|b| b.len() == cell)?;
                    Some(Tile { tile: t, profile: DomainId::parse(&t.colour_profile_id).ok()?, body })
                })
                .collect();
            if let Some(r) = resolved.filter(|r| !r.is_empty()) {
                layers.insert(l.layer_id.as_str(), r);
            }
        }
        let working = match &doc.working_space_rgb {
            ProfileBinding::Assigned(id) => Some(id.as_str()),
            ProfileBinding::Unassigned(()) => None,
        };
        Self { layers, working, profile_sha256: profile_hash(WORKING_PROFILE_LINEAR_SRGB) }
    }
}

impl PdfAssets for LayoutAssets<'_> {
    fn path(&self, _: &str) -> Option<OwnedPath> {
        None
    }

    fn raster(&self, layer_id: &str) -> Option<PlacedRaster<'_>> {
        let tiles = self.layers.get(layer_id)?;
        let edge = u32::try_from(CELL).ok()?;
        let stride = |bytes: u32| u64::from(edge).checked_mul(u64::from(bytes));
        let (colour_stride, alpha_stride) = (stride(12)?, stride(4)?);
        let grid = Grid { origin_x: 0, origin_y: 0, tile_width: edge, tile_height: edge };
        let mut out = Vec::with_capacity(tiles.len());
        for t in tiles {
            let bounds = grid.bounds(t.tile.column, t.tile.row).unwrap_or(Rect { x: 0, y: 0, width: 0, height: 0 });
            let (colour, alpha) = t.body.split_at(COLOUR_CELL.min(t.body.len()));
            out.push(ResolvedTile {
                tile_ref: t.tile,
                bounds,
                sample_format: "rgb_f32le",
                colour_bytes: colour,
                colour_stride_bytes: colour_stride,
                colour_sha256: profile_hash(colour),
                alpha_bytes: alpha,
                alpha_stride_bytes: alpha_stride,
                alpha_sha256: profile_hash(alpha),
                transfer: "linear_light",
                alpha_association: "straight",
                profile: ProfileInput { profile_id: &t.profile, bytes: WORKING_PROFILE_LINEAR_SRGB, expected_sha256: self.profile_sha256 },
            });
        }
        // The origin and scale are unused for a graphic frame (the frame's fitting transform places the tiles).
        Some(PlacedRaster { grid, tiles: out, origin_pt: [0.0, 0.0], pt_per_px: 1.0 })
    }

    fn profile(&self, profile_id: &str) -> Option<&[u8]> {
        (self.working == Some(profile_id)).then_some(WORKING_PROFILE_LINEAR_SRGB)
    }
}
