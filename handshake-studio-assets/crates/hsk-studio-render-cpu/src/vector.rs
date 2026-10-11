//! The CPU backend's vector payload `V` for the shared render plan (`PlanNode::Vector(V)`): a nib
//! `OwnedPath` (nib owns vector geometry, CX-CAP-001) with its fill colour. `Serialize` exists only
//! so `RenderPlan::digest` can bind receipts to the exact plan; the path is bound through nib's
//! canonical JSON text.
use hsk_studio_nib::OwnedPath;
use hsk_studio_pigment::render_plan::NoVector;
use serde::{Serialize, Serializer};

/// Filled path. Path coordinates are plan pixels; build them from anchors with
/// `OwnedPath::from_anchors(.., Unit::Pixels, ppi, ..)`. Open subpaths are filled as if closed by
/// a straight segment.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct VectorFill {
    #[serde(serialize_with = "serialize_path")]
    pub path: OwnedPath,
    /// Straight RGBA fill colour, each channel in `[0, 1]`.
    pub rgba: [f32; 4],
    /// Samples per axis for coverage, `1..=16` (coverage resolution is `1/n^2`).
    pub aa_samples: u8,
}

/// Payload types the CPU backend can draw. `NoVector` is uninhabited, so plans without vectors
/// stay non-generic in practice.
pub trait VectorSource {
    fn fill(&self) -> &VectorFill;
}
impl VectorSource for VectorFill {
    fn fill(&self) -> &VectorFill {
        self
    }
}
impl VectorSource for NoVector {
    fn fill(&self) -> &VectorFill {
        match *self {}
    }
}

fn serialize_path<S: Serializer>(path: &OwnedPath, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(&path.to_canonical_json())
}
