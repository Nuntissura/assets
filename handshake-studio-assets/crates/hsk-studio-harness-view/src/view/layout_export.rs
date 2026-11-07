//! Layout output from the host (lane SMB-LAYOUT-PDF-HOST-1; STU-LAY-054, STU-LAY-061, STU-LAY-154,
//! STU-LAY-158). Red: the API the tests drive, every method refuses `not_implemented`.
#![deny(clippy::arithmetic_side_effects)]
use super::Viewer;
use serde_json::Value;
use std::path::Path;

/// The PDF/X standard member of [`LayoutPdfSettings::pdfx`] (press's).
pub use hsk_studio_press::PdfXStandard;

/// Largest output-intent ICC file read.
pub const MAX_PROFILE_BYTES: u64 = 4 << 20;

/// A typed refusal: a stable code and a detail.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LayoutExportRefusal {
    pub code: &'static str,
    pub detail: String,
}

impl std::fmt::Display for LayoutExportRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.code, self.detail)
    }
}

type Result<T> = std::result::Result<T, LayoutExportRefusal>;

fn red<T>() -> Result<T> {
    Err(LayoutExportRefusal { code: "not_implemented", detail: String::new() })
}

/// PDF export settings.
#[derive(Clone, Copy, Debug)]
pub struct LayoutPdfSettings<'a> {
    pub pdfx: PdfXStandard,
    pub created: Option<&'a str>,
    pub output_profile: Option<&'a [u8]>,
    pub output_condition_identifier: Option<&'a str>,
    pub output_condition: Option<&'a str>,
    pub registry: Option<&'a str>,
    pub title: Option<&'a str>,
    pub subset_fonts: bool,
    pub ignore_preflight: bool,
}

impl Default for LayoutPdfSettings<'_> {
    fn default() -> Self {
        Self { pdfx: PdfXStandard::None, created: None, output_profile: None, output_condition_identifier: None, output_condition: None, registry: None, title: None, subset_fonts: true, ignore_preflight: false }
    }
}

/// Reads at most `limit` bytes of `path`, typed.
pub fn read_bounded(_path: &Path, _limit: u64, _code: &'static str) -> Result<Vec<u8>> {
    red()
}

impl Viewer {
    pub fn layout_open(&mut self, _page_size_pt: Option<(f64, f64)>, _pages: Option<u32>) -> Result<Value> {
        red()
    }
    pub fn layout_preflight(&mut self) -> Result<Value> {
        red()
    }
    pub fn export_layout_pdf(&mut self, _path: &Path, _overwrite: bool, _settings: &LayoutPdfSettings<'_>) -> Result<Value> {
        red()
    }
    pub fn export_layout_idml(&mut self, _path: &Path, _overwrite: bool) -> Result<Value> {
        red()
    }
    pub fn render_pdf_page(&mut self, _source: &Path, _page: usize, _px_per_pt: f64, _out: &Path, _overwrite: bool) -> Result<Value> {
        red()
    }
}
