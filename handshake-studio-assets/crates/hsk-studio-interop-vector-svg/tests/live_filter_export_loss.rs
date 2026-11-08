//! Lane SMB-AI-LIVE-EFFECTS-1 (V4-06 sweep, item 5): the direct record exporters (SVG
//! `export_proposal_with_loss`, EPS `export_eps_proposal` of hsk-studio-interop-vector-core) write a
//! vector layer's raw paths, so an enabled live filter stack (`StudioVectorRecords::live_filters`) and
//! an envelope (`StudioEnvelope`) are typed losses, never a silent drop. The envelope loss existed
//! before this lane (found by the sweep and reported). The Viewer's own SVG and PDF exports go
//! through composite's `expand_vectors` and draw both. Synthetic SVG only.
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{Payload, StudioEnvelope, StudioEnvelopeRaster, StudioEnvelopeShape, StudioLiveFilter, StudioLiveFilterParam, StudioLiveFilterStack, StudioWarpAxis, StudioWarpStyle};
use hsk_studio_interop_vector_core::{ExportOptions, ImportPolicy, LossKind, SequentialIdMint, VectorProposal, export_eps_proposal};
use hsk_studio_interop_vector_svg::{export_proposal_with_loss, import_svg};

fn proposal() -> VectorProposal {
    let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100"><rect x="10" y="10" width="40" height="20" fill="#f00"/></svg>"##;
    import_svg(svg.as_bytes(), &ImportPolicy::default(), &mut SequentialIdMint::new(7), &CancellationToken::default()).unwrap()
}

fn vector_layer(p: &VectorProposal) -> String {
    p.snapshot.document().layers.iter().find(|l| matches!(l.payload, Payload::Vector { .. })).expect("a vector layer").layer_id.clone()
}

fn twist(enabled: bool) -> StudioLiveFilter {
    StudioLiveFilter {
        instance_id: "lf1".into(),
        filter_kind: "hsk.studio.vector_effect.twist".into(),
        version: 1,
        enabled,
        params: vec![StudioLiveFilterParam::Real { key: "twist_angle".into(), value: 30.0 }],
    }
}

fn envelope(layer_id: String) -> StudioEnvelope {
    StudioEnvelope {
        layer_id,
        envelope: StudioEnvelopeShape::Warp { warp_style: StudioWarpStyle::Arc, warp_axis: StudioWarpAxis::Horizontal, warp_bend: 0.2, warp_distortion_horizontal: 0.0, warp_distortion_vertical: 0.0 },
        envelope_fidelity: 50.0,
        envelope_raster_mode: StudioEnvelopeRaster::ClippingMask,
        anti_alias: false,
        distort_appearance: false,
        distort_linear_gradient_fills: false,
        distort_pattern_fills: false,
    }
}

/// `(kind, code)` of the SVG and the EPS export losses.
fn losses(p: &VectorProposal) -> (Vec<(LossKind, &'static str)>, Vec<(LossKind, &'static str)>) {
    let svg = export_proposal_with_loss(p, &ExportOptions::default()).unwrap();
    let eps = export_eps_proposal(p, &ExportOptions::default()).unwrap();
    (svg.loss.iter().map(|l| (l.kind, l.code)).collect(), eps.loss.iter().map(|l| (l.kind, l.code)).collect())
}

#[test]
fn live_filters_and_envelopes_are_typed_losses_of_the_svg_and_eps_exports() {
    let mut p = proposal();
    let layer = vector_layer(&p);
    let filter = (LossKind::Unsupported, "live_filter_not_exported");
    let warp = (LossKind::Unsupported, "envelope_not_exported");
    let (svg, eps) = losses(&p);
    assert!(!svg.contains(&filter) && !svg.contains(&warp) && !eps.contains(&filter) && !eps.contains(&warp), "{svg:?} {eps:?}");
    // A stack of disabled entries draws like none: not a loss.
    p.vector_records.live_filters.push(StudioLiveFilterStack { layer_id: layer.clone(), filters: vec![twist(false)] });
    let (svg, eps) = losses(&p);
    assert!(!svg.contains(&filter) && !eps.contains(&filter), "{svg:?} {eps:?}");
    p.vector_records.live_filters[0].filters[0].enabled = true;
    let (svg, eps) = losses(&p);
    assert!(svg.contains(&filter) && eps.contains(&filter), "{svg:?} {eps:?}");
    p.vector_records.envelopes.push(envelope(layer));
    let (svg, eps) = losses(&p);
    assert!(svg.contains(&warp) && eps.contains(&warp), "{svg:?} {eps:?}");
}
