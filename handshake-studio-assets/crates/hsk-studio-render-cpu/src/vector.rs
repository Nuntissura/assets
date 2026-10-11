//! The CPU backend's vector payload `V` for the shared render plan (`PlanNode::Vector(V)`): a nib
//! anchor path with its fill. Until nib owns a serializable owned path type this lives here;
//! `Serialize` exists only so `RenderPlan::digest` can bind receipts to the exact plan.
use hsk_studio_nib::{Anchor, Point, Winding};
use hsk_studio_pigment::render_plan::NoVector;
use serde::{Serialize, Serializer};

/// Filled closed (or implicitly closed) path. Coordinates are pixels, or points/mm/in together
/// with `pixels_per_inch`; handles are vectors relative to their anchor (nib contract).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct VectorFill {
    #[serde(serialize_with = "serialize_anchors")]
    pub anchors: Vec<Anchor>,
    pub closed: bool,
    #[serde(serialize_with = "serialize_winding")]
    pub winding: Winding,
    /// Straight RGBA fill colour, each channel in `[0, 1]`.
    pub rgba: [f32; 4],
    /// Samples per axis for coverage, `1..=16` (coverage resolution is `1/n^2`).
    pub aa_samples: u8,
    pub pixels_per_inch: Option<f64>,
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

#[derive(Serialize)]
struct PointWire {
    x: f64,
    y: f64,
    x_unit: &'static str,
    y_unit: &'static str,
}
#[derive(Serialize)]
struct AnchorWire {
    position: PointWire,
    incoming: PointWire,
    outgoing: PointWire,
}
fn point(p: Point) -> PointWire {
    PointWire {
        x: p.x.value(),
        y: p.y.value(),
        x_unit: p.x.unit().as_str(),
        y_unit: p.y.unit().as_str(),
    }
}
fn serialize_anchors<S: Serializer>(anchors: &[Anchor], s: S) -> Result<S::Ok, S::Error> {
    s.collect_seq(anchors.iter().map(|a| AnchorWire {
        position: point(a.position),
        incoming: point(a.incoming),
        outgoing: point(a.outgoing),
    }))
}
fn serialize_winding<S: Serializer>(winding: &Winding, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(winding.wire())
}
