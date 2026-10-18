//! Album embed renderer (WP-KERNEL-012 MT-014).
//!
//! An `album` embed renders its members as a WRAPPING grid of thumbnails (3 per row by
//! default — AC-6). Clicking a thumbnail opens a full-size modal (`egui::Window`) showing that
//! member. The modal's open member is held in [`AlbumViewState`], stored per embed node in
//! `RichEditorState` so the modal survives across frames until dismissed.
//!
//! AccessKit author_ids (the AC-6 contract ids):
//!   - grid container: `album-{ref_value}`
//!   - each cell: `album-cell-{asset_id}`

/// Default number of thumbnails per row in the album grid (AC-6).
pub const ALBUM_COLUMNS: usize = 3;

/// Per-embed album view state: which member (by index) is currently open in the full-size
/// modal, or `None` when the modal is closed. Stored per embed node (keyed by ref_value).
#[derive(Debug, Clone, Default)]
pub struct AlbumViewState {
    /// The index of the member shown full-size in the modal, or `None` if the modal is closed.
    pub open_index: Option<usize>,
}

impl AlbumViewState {
    /// A fresh state with the modal closed.
    pub fn new() -> Self {
        Self { open_index: None }
    }

    /// Open the modal on member `index` (clamped to `0..len`; a no-op when `len == 0`).
    pub fn open(&mut self, index: usize, len: usize) {
        if len == 0 {
            self.open_index = None;
        } else {
            self.open_index = Some(index.min(len - 1));
        }
    }

    /// Close the modal.
    pub fn close(&mut self) {
        self.open_index = None;
    }

    /// True when the modal is open.
    pub fn is_open(&self) -> bool {
        self.open_index.is_some()
    }
}

/// The AccessKit author_id for the album grid container (AC-6: `album-{ref_value}`). The
/// `ref_value` is the raw comma-list (matching the React `album-{ref_value}` id), so the
/// container is addressable by its exact document ref.
pub fn grid_author_id(ref_value: &str) -> String {
    format!("album-{ref_value}")
}

/// The AccessKit author_id for one album cell (AC-6: `album-cell-{asset_id}`).
pub fn cell_author_id(asset_id: &str) -> String {
    format!("album-cell-{asset_id}")
}

/// The number of rows a `count`-member album occupies at `columns` per row (ceil division).
/// Used to reserve grid height; pure arithmetic, unit-tested with no GPU.
pub fn row_count(count: usize, columns: usize) -> usize {
    let cols = columns.max(1);
    count.div_ceil(cols)
}

/// The `(row, col)` grid position of the `index`-th member at `columns` per row.
pub fn grid_position(index: usize, columns: usize) -> (usize, usize) {
    let cols = columns.max(1);
    (index / cols, index % cols)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn three_per_row_grid_layout_ac6() {
        // AC-6: 3 thumbnails per row. 7 members -> 3 rows (3+3+1).
        assert_eq!(row_count(7, ALBUM_COLUMNS), 3);
        assert_eq!(row_count(3, ALBUM_COLUMNS), 1);
        assert_eq!(row_count(0, ALBUM_COLUMNS), 0);
        assert_eq!(row_count(4, ALBUM_COLUMNS), 2);
        // Member positions wrap every 3 columns.
        assert_eq!(grid_position(0, ALBUM_COLUMNS), (0, 0));
        assert_eq!(grid_position(2, ALBUM_COLUMNS), (0, 2));
        assert_eq!(grid_position(3, ALBUM_COLUMNS), (1, 0));
        assert_eq!(grid_position(5, ALBUM_COLUMNS), (1, 2));
    }

    #[test]
    fn modal_open_close_clamps() {
        let mut s = AlbumViewState::new();
        assert!(!s.is_open());
        s.open(1, 3);
        assert_eq!(s.open_index, Some(1));
        assert!(s.is_open());
        // Opening past the end clamps to the last member.
        s.open(9, 3);
        assert_eq!(s.open_index, Some(2));
        // Opening on an empty album closes the modal.
        s.open(0, 0);
        assert_eq!(s.open_index, None);
        s.open(1, 3);
        s.close();
        assert!(!s.is_open());
    }

    #[test]
    fn author_ids_match_ac6_contract() {
        assert_eq!(grid_author_id("a1,a2,a3"), "album-a1,a2,a3");
        assert_eq!(cell_author_id("a2"), "album-cell-a2");
    }
}
