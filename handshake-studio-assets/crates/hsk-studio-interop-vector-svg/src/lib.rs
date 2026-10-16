//! SVG half of STUDIO-MODULE-INTEROP_VECTOR (SMB-CRATE-SEAM-SPLITS slice 3a): SVG/SVGZ import
//! (`import_svg`), SVG export and gzip, on the core half's model, ids and errors.
//! `hsk-studio-interop-vector` is the public face.
#![forbid(unsafe_code)]

// The core half's modules under the names these modules have always used (`crate::model`, ...).
use hsk_studio_interop_vector_core::{error, ids, model};

mod gzip;
mod svg_adapter;
mod svg_export;

pub use gzip::gzip_svg;
pub use svg_adapter::import_svg;
pub use svg_export::{
    SvgExport, export_proposal, export_proposal_with_loss, export_svg, export_svg_with_loss,
};
