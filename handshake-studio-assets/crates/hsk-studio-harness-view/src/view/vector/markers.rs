//! Stroke arrowheads in the viewer (lane SMB-AI-ARROWHEADS-2; STU-VEC-146): the start and end
//! markers of one stroke row of a vector layer's appearance, as Folio `stroke_markers` entries in
//! the vector entry's records (`style_id` = the layer's own appearance style, `row` = the index of
//! the stroke row in its rows). [`super::StrokePanel`] gains no field (struct literals in other
//! crates); markers are a separate [`MarkerSpec`] and separate methods.
//!
//! Each set or swap is ONE undo step, the same entry-only history step as the style edits
//! ([`super::style`]): the layer's appearance becomes its own style (a linked graphic style is
//! broken into a local copy), the entry is re-validated by Folio `validate_vector_records`, then
//! committed. Markers follow their stroke row through every restyle by row kind and per-kind
//! ordinal ([`super::style`] `rebind`). Composite's Folio adapter draws them (nib's marker table, a
//! Studio reading, UNVERIFIED D5); the six `ERD_*` connector terminators are stored, not drawn, so
//! the viewer never sets one (an imported one is kept). The swap is Folio's `swap_markers`.
#![deny(clippy::arithmetic_side_effects)]
use super::super::Viewer;
use super::style;
use hsk_studio_folio::{self as folio, StudioAppearanceRow, StudioArrowAlignment, StudioArrowhead, StudioArrowheadSuite};
use serde_json::{Value, json};

/// The markers of one stroke row: start and end tokens, their scales (percent of the stroke
/// weight, `1..=1000`) and the alignment of both ends.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MarkerSpec {
    pub start: StudioArrowhead,
    pub end: StudioArrowhead,
    pub start_scale: f64,
    pub end_scale: f64,
    pub alignment: StudioArrowAlignment,
}

impl MarkerSpec {
    /// Both scales at Folio's `DEFAULT_MARKER_SCALE` (100, `studio_declared`).
    pub fn new(start: StudioArrowhead, end: StudioArrowhead, alignment: StudioArrowAlignment) -> Self {
        Self { start, end, start_scale: folio::DEFAULT_MARKER_SCALE, end_scale: folio::DEFAULT_MARKER_SCALE, alignment }
    }

    /// The spec of a Folio entry (`None` for a token outside the catalogue, which Folio refuses).
    pub fn of(m: &folio::StudioStrokeMarkers) -> Option<Self> {
        Some(Self { start: m.start_marker()?, end: m.end_marker()?, start_scale: m.start_scale, end_scale: m.end_scale, alignment: m.alignment })
    }

    /// The Folio entry for stroke row `row` of style `style_id`.
    pub fn entry(&self, style_id: &str, row: u32) -> folio::StudioStrokeMarkers {
        folio::StudioStrokeMarkers {
            style_id: style_id.to_string(),
            row,
            start: self.start.token().to_string(),
            end: self.end.token().to_string(),
            start_scale: self.start_scale,
            end_scale: self.end_scale,
            alignment: self.alignment,
        }
    }
}

/// Refuses hostile scales and a newly chosen connector terminator (stored, not drawn) before
/// anything is built; Folio validates the entry again.
fn check(op: &str, spec: &MarkerSpec, current: Option<&MarkerSpec>) -> Result<(), String> {
    for (v, field) in [(spec.start_scale, "start_scale"), (spec.end_scale, "end_scale")] {
        if !(v.is_finite() && (folio::MARKER_SCALE_MIN..=folio::MARKER_SCALE_MAX).contains(&v)) {
            return Err(format!("{op}: {field} {v} outside 1..=1000 %"));
        }
    }
    for (token, kept) in [(spec.start, current.map(|c| c.start)), (spec.end, current.map(|c| c.end))] {
        if token.suite() == StudioArrowheadSuite::Connector && kept != Some(token) {
            return Err(format!("{op}: marker_shape_unbuilt ({} is stored, not drawn)", token.token()));
        }
    }
    Ok(())
}

/// Row `row` of `rows` must be a stroke row.
fn stroke_row(op: &str, rows: &[StudioAppearanceRow], row: u32) -> Result<(), String> {
    match usize::try_from(row).ok().and_then(|i| rows.get(i)) {
        Some(StudioAppearanceRow::Stroke { .. }) => Ok(()),
        Some(_) => Err(format!("{op}: row {row} is not a stroke row")),
        None => Err(format!("{op}: no row {row} (no stroke row there)")),
    }
}

impl Viewer {
    /// The markers of stroke row `row` of `layer_id`'s appearance (`None`: no markers). Refused
    /// when that row is not a stroke row.
    pub fn stroke_markers(&self, layer_id: &str, row: u32) -> Result<Option<MarkerSpec>, String> {
        let record = self.vector_path(layer_id)?;
        let entry = self.vector_entry()?;
        let (rows, from) = style::current_rows(&entry, self.vdoc()?, layer_id, &record)?;
        stroke_row("stroke_markers", &rows, row)?;
        Ok(from.and_then(|id| entry.records.stroke_marker(&id, row).and_then(MarkerSpec::of)))
    }

    /// The index of the first stroke row of `layer_id`'s appearance (the Stroke panel's row).
    pub fn first_stroke_row(&self, layer_id: &str) -> Result<u32, String> {
        let rows = self.layer_paint_style(layer_id)?.rows;
        let i = rows.iter().position(|r| matches!(r, StudioAppearanceRow::Stroke { .. })).ok_or_else(|| format!("{layer_id} has no stroke"))?;
        u32::try_from(i).map_err(|_| "stroke row index".to_string())
    }

    /// Sets the markers of stroke row `row` of `layer_id` (`None` removes them): ONE undo step,
    /// validated by Folio before the commit.
    pub fn set_stroke_markers(&mut self, layer_id: &str, row: u32, spec: Option<MarkerSpec>) -> Result<String, String> {
        let r = (|| {
            let op = "set_stroke_markers";
            let current = self.stroke_markers(layer_id, row).map_err(|e| e.replacen("stroke_markers", op, 1))?;
            if let Some(s) = &spec {
                check(op, s, current.as_ref())?;
            }
            let mut edit = |records: &mut folio::StudioVectorRecords, style_id: &str, rows: &[StudioAppearanceRow]| -> Result<(), String> {
                stroke_row(op, rows, row)?;
                records.stroke_markers.retain(|m| !(m.style_id == style_id && m.row == row));
                if let Some(s) = spec {
                    records.stroke_markers.push(s.entry(style_id, row));
                }
                Ok(())
            };
            self.restyle_with(op, layer_id, None, |_, _| Ok(None), Some(&mut edit))
        })();
        self.report(r)
    }

    /// The swap command (STU-VEC-146, Folio `swap_markers`): start and end tokens and scales
    /// exchanged, alignment kept. ONE undo step.
    pub fn swap_stroke_markers(&mut self, layer_id: &str, row: u32) -> Result<String, String> {
        let r = (|| {
            let op = "swap_stroke_markers";
            if self.stroke_markers(layer_id, row).map_err(|e| e.replacen("stroke_markers", op, 1))?.is_none() {
                return Err(format!("{op}: row {row} has no markers"));
            }
            let mut edit = |records: &mut folio::StudioVectorRecords, style_id: &str, rows: &[StudioAppearanceRow]| -> Result<(), String> {
                stroke_row(op, rows, row)?;
                let m = records.stroke_markers.iter_mut().find(|m| m.style_id == style_id && m.row == row).ok_or_else(|| format!("{op}: row {row} has no markers"))?;
                *m = folio::swap_markers(m);
                Ok(())
            };
            self.restyle_with(op, layer_id, None, |_, _| Ok(None), Some(&mut edit))
        })();
        self.report(r)
    }

    /// The Folio marker entries of `layer_id`'s own appearance style (`state_json` projection):
    /// `[{row, start, end, start_scale, end_scale, alignment}]`, empty without markers.
    pub fn stroke_markers_json(&self, layer_id: &str) -> Value {
        let (Ok(entry), Ok(style)) = (self.vector_entry(), self.layer_paint_style(layer_id)) else {
            return json!([]);
        };
        let list: Vec<Value> = entry
            .records
            .stroke_markers
            .iter()
            .filter(|m| m.style_id == style.style_id)
            .map(|m| json!({"row": m.row, "start": m.start, "end": m.end, "start_scale": m.start_scale, "end_scale": m.end_scale, "alignment": m.alignment}))
            .collect();
        Value::Array(list)
    }
}
