//! Smart-filter stacks on the lowering path (lane SMB-PS-SMART-FILTER-WIRING): the Folio record
//! `StudioRasterLayerRecord::smart_filters` becomes pigment's envelope entry
//! `LayerExtension::smart_filters` through pigment's own converter
//! `smart_filter::SmartFilterOp::from_record` (single owner of the mapping). Each enabled
//! per-filter mask and the stack mask are registered as plan mask sources (coverage tiles of the
//! layer, the same table as layer masks: format, placement and hash from the caller's
//! `TileInfoSource`); a disabled mask is dropped (the filter applies everywhere). A disabled stack
//! or one without enabled entries lowers to nothing. A stack pigment would refuse is a typed
//! lowering error (`<layer>:smart_filter_parameter`), so lowering and `admit_extended` agree.
//! Rendering: render-cpu runs the stack bit for bit like pigment; render-gpu has a pass within
//! tolerance for filter-only stacks (see render-gpu `smart_filter`).
#![deny(clippy::arithmetic_side_effects)]
use super::{Emitter, mask_format, tile_rects, union};
use crate::CompositeError;
use hsk_studio_folio::{StudioLayer, StudioMask, StudioRasterRecords, TileRef};
use hsk_studio_pigment::render_plan::{MaskInput, PlanError};
use hsk_studio_pigment::smart_filter::SmartFilterOp;

impl Emitter<'_> {
    /// A coverage-tile mask of `layer_id` as a plan mask source (density 1, not inverted).
    fn smart_mask(&mut self, layer_id: &str, tiles: &[TileRef]) -> Result<MaskInput, CompositeError> {
        if tiles.is_empty() {
            return Err(CompositeError::Unavailable(format!("{layer_id}:smart_filter_mask_empty")));
        }
        let format = mask_format(tiles)?;
        let mut placement = None;
        for rect in tile_rects(tiles)? {
            placement = union(placement, rect)?;
        }
        let placement = placement.ok_or(CompositeError::Plan(PlanError::Empty))?;
        let sha256 = self
            .info
            .mask_sha256(tiles)
            .ok_or_else(|| CompositeError::Unavailable(format!("{layer_id}/smart_filters:mask_hash")))?;
        let key = self.key()?;
        self.mask_sources.push((key, tiles.to_vec()));
        Ok(MaskInput { key, placement, format, sha256, inverted: false, density: 1.0 })
    }

    /// The envelope op of `layer`'s smart-filter record, if any.
    pub(super) fn smart_filters(
        &mut self,
        records: Option<&StudioRasterRecords>,
        layer: &StudioLayer,
    ) -> Result<Option<SmartFilterOp>, CompositeError> {
        let Some(stack) = records
            .and_then(|r| r.layer(&layer.layer_id))
            .and_then(|r| r.smart_filters.as_ref())
        else {
            return Ok(None);
        };
        // Disabled masks apply everywhere: drop them before conversion.
        let mut stack = stack.clone();
        let keep = |m: &Option<StudioMask>| m.as_ref().filter(|m| m.enabled).cloned();
        stack.stack_mask = keep(&stack.stack_mask);
        for f in &mut stack.filters {
            f.mask = keep(&f.mask);
        }
        let id = layer.layer_id.clone();
        let mut failure = None;
        let op = SmartFilterOp::from_record(&stack, |m| match self.smart_mask(&id, &m.tiles) {
            Ok(mask) => Some(mask),
            Err(e) => {
                failure.get_or_insert(e);
                None
            }
        });
        if let Some(e) = failure {
            return Err(e);
        }
        let op = op.map_err(|_| CompositeError::Unavailable(format!("{id}:smart_filter_mask")))?;
        if op.entries.iter().all(|e| !e.enabled) {
            return Ok(None);
        }
        if !op.check() {
            return Err(CompositeError::Unavailable(format!("{id}:smart_filter_parameter")));
        }
        Ok(Some(op))
    }
}
