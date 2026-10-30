//! Auto layout in the window (lane SMB-FIG-GRID-2; [STU-DS-144..150], [STU-DS-160], [STU-CMP-110]).
//! Red commit: the command surface is declared and every command is refused (`not_implemented`);
//! the section is never drawn.
#![deny(clippy::arithmetic_side_effects)]
use super::super::Viewer;
use hsk_studio_components::GridTrackMove;
use hsk_studio_folio::{ChildSizing, GridAutoTracks, GridItemsPositioning, LayoutMode, StudioAutoLayout, StudioChildLayout, StudioGridChild, StudioGridTrackSize};
use serde_json::{Value, json};

/// Stable widget ids of the auto-layout section.
pub mod widget {
    pub const SECTION: &str = "design.auto_layout";
    pub const SPACING: &str = "design.auto_layout.spacing";
    pub const ROWS: &str = "design.auto_layout.grid.rows";
    pub const COLUMNS: &str = "design.auto_layout.grid.columns";
    pub const ROW_GAP: &str = "design.auto_layout.grid.row_gap";
    pub const COLUMN_GAP: &str = "design.auto_layout.grid.column_gap";
    pub const AUTO_TRACKS: &str = "design.auto_layout.grid.auto_tracks";
    pub const ITEMS_POSITIONING: &str = "design.auto_layout.grid.items_positioning";
    pub const CHILD_ANCHORED: &str = "design.auto_layout.child.anchored";
    pub const ERROR: &str = "design.auto_layout.error";
    pub fn mode(token: &str) -> String {
        format!("design.auto_layout.mode.{token}")
    }
    pub fn padding(side: &str) -> String {
        format!("design.auto_layout.padding.{side}")
    }
    pub fn track(axis: &str, index: usize, field: &str) -> String {
        format!("design.auto_layout.grid.{axis}.{index}.{field}")
    }
    pub fn child(field: &str) -> String {
        format!("design.auto_layout.child.{field}")
    }
    pub fn sizing(axis: &str) -> String {
        format!("design.auto_layout.child.sizing.{axis}")
    }
}

pub const SECTION_TRACKS: usize = 64;
pub const MAX_SUBTREE: usize = 4096;

#[derive(Clone, Debug, PartialEq)]
pub struct AutoLayoutView {
    pub node: String,
    pub frame: bool,
    pub auto_layout: Option<StudioAutoLayout>,
    pub child_layout: Option<StudioChildLayout>,
    pub parent: Option<(String, StudioAutoLayout)>,
}

impl AutoLayoutView {
    pub fn json(&self) -> Value {
        json!({"node": self.node})
    }
}

fn stub() -> Result<(), String> {
    Err("auto_layout: not_implemented".into())
}

impl Viewer {
    pub fn auto_layout_view(&self, _layer_id: &str) -> Option<AutoLayoutView> {
        None
    }
    pub fn auto_layout_json(&self) -> Value {
        json!({"selected": null, "last": null})
    }
    pub fn set_auto_layout_mode(&mut self, _node: &str, _mode: LayoutMode) -> Result<(), String> {
        stub()
    }
    pub fn set_auto_layout_spacing(&mut self, _node: &str, _item_spacing: f64, _padding: [f64; 4]) -> Result<(), String> {
        stub()
    }
    pub fn set_grid_tracks(&mut self, _node: &str, _rows: &[StudioGridTrackSize], _columns: &[StudioGridTrackSize]) -> Result<(), String> {
        stub()
    }
    pub fn set_grid_gaps(&mut self, _node: &str, _row_gap: f64, _column_gap: f64) -> Result<(), String> {
        stub()
    }
    pub fn set_grid_options(&mut self, _node: &str, _auto_tracks: GridAutoTracks, _positioning: GridItemsPositioning) -> Result<(), String> {
        stub()
    }
    pub fn set_grid_child(&mut self, _node: &str, _placement: Option<StudioGridChild>) -> Result<(), String> {
        stub()
    }
    pub fn set_child_sizing(&mut self, _node: &str, _horizontal: ChildSizing, _vertical: ChildSizing) -> Result<(), String> {
        stub()
    }
    pub fn grid_append_child_at(&mut self, _container: &str, _node: &str, _row: u32, _column: u32) -> Result<(), String> {
        stub()
    }
    pub fn grid_reorder_rows(&mut self, _container: &str, _options: GridTrackMove) -> Result<(), String> {
        stub()
    }
    pub fn grid_reorder_columns(&mut self, _container: &str, _options: GridTrackMove) -> Result<(), String> {
        stub()
    }
    pub fn auto_layout_control(&mut self, _widget_id: &str, _value: &str) -> Result<(), String> {
        stub()
    }
    pub(in crate::view) fn auto_layout_ui(&mut self, _ui: &mut egui::Ui) {}
}
