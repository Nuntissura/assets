//! Lane SMB-AI-LIVE-EFFECTS-1 (V4-06 sweep, item 5): the frame PDF export writes a vector layer's
//! raw paths, so a live filter stack (`StudioVectorRecords::live_filters`, drawn by composite) and an
//! envelope (`StudioEnvelope`, also drawn by composite) are typed losses of the export, never a
//! silent drop. The envelope loss existed before this lane (found by the sweep and reported). The
//! node is the `fig-basic-shapes.fig` fixture of `hsk-studio-interop-fig`, as in `fig_markers.rs`.
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{
    Budget, NodeKind, NodeRef, Resolution, Resolver, StudioEnvelope, StudioEnvelopeRaster, StudioEnvelopeShape, StudioLiveFilter, StudioLiveFilterParam,
    StudioLiveFilterStack, StudioWarpAxis, StudioWarpStyle, TileRef,
};
use hsk_studio_interop_fig_base::{FigOutcome, Limits, VersionPolicy, decode_document, import_fig};
use hsk_studio_interop_fig_native::{FigExportFormat, FigExportPlan, NativeConfig, NativeProposal, SequentialIdMint, export_frame_pdf, to_native};

struct Host;
impl Resolver for Host {
    fn primitive(&self, _: &str, _: &str) -> Resolution {
        Resolution::Absent
    }
    fn profile(&self, _: &str) -> Resolution {
        Resolution::Absent
    }
    fn tile(&self, _: &TileRef) -> Resolution {
        Resolution::Absent
    }
    fn format_supported(&self, _: &str) -> bool {
        false
    }
    fn primitive_layer_binding(&self, kind: &str, schema: &str) -> bool {
        kind == "component_instance" && schema == hsk_studio_accord::SCHEMA_STUDIO_COMPONENT_INSTANCE
    }
}

fn proposal() -> NativeProposal {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../hsk-studio-interop-fig/tests/fixtures/fig-basic-shapes.fig");
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("fixture {}: {e}", path.display()));
    let cancel = CancellationToken::default();
    let FigOutcome::Imported(import) = import_fig(&bytes, &Limits::default(), &VersionPolicy::observed(), None, &cancel).unwrap() else {
        panic!("fixture imports")
    };
    let doc = decode_document(&import, &Limits::default(), &cancel).unwrap();
    let cfg = NativeConfig { budget: Budget::default(), resolver: &Host };
    to_native(&doc, &cfg, &mut SequentialIdMint::new(0xA11), &cancel).unwrap()
}

/// The losses of a PDF export of layer `line-outline` (its own frame), as `(path, code)`.
fn losses(p: &NativeProposal) -> Vec<(String, &'static str)> {
    let line = p.snapshot.document().layers.iter().find(|l| l.name == "line-outline").expect("layer").layer_id.clone();
    let plan = FigExportPlan {
        source: p.id_map.iter().find(|e| e.node.node_id == line).expect("source").source,
        frame: NodeRef { node_kind: NodeKind::Layer, node_id: line },
        file_name: "line.pdf".into(),
        format: FigExportFormat::Pdf,
        scale: 1.0,
        bounds: [0.0, 0.0, 400.0, 400.0],
        default_setting: false,
    };
    let pdf = export_frame_pdf(p, &plan, None, &CancellationToken::default()).unwrap();
    pdf.loss.iter().map(|l| (l.path.clone(), l.code)).collect()
}

#[test]
fn live_filters_and_envelopes_are_typed_losses_of_the_frame_export() {
    let mut p = proposal();
    let line = p.snapshot.document().layers.iter().find(|l| l.name == "line-outline").unwrap().layer_id.clone();
    let has = |l: &[(String, &'static str)], code: &str| l.iter().any(|(_, c)| *c == code);
    let before = losses(&p);
    assert!(!has(&before, "live_filter_not_exported") && !has(&before, "envelope_not_exported"), "{before:?}");
    let twist = |enabled: bool| StudioLiveFilter {
        instance_id: "lf1".into(),
        filter_kind: "hsk.studio.vector_effect.twist".into(),
        version: 1,
        enabled,
        params: vec![StudioLiveFilterParam::Real { key: "twist_angle".into(), value: 30.0 }],
    };
    // A stack of disabled entries draws like none: not a loss.
    p.vector_records.live_filters.push(StudioLiveFilterStack { layer_id: line.clone(), filters: vec![twist(false)] });
    assert!(!has(&losses(&p), "live_filter_not_exported"));
    p.vector_records.live_filters[0].filters[0].enabled = true;
    let with_filter = losses(&p);
    assert!(with_filter.contains(&("export.live_filters".into(), "live_filter_not_exported")), "{with_filter:?}");
    p.vector_records.envelopes.push(StudioEnvelope {
        layer_id: line,
        envelope: StudioEnvelopeShape::Warp { warp_style: StudioWarpStyle::Arc, warp_axis: StudioWarpAxis::Horizontal, warp_bend: 0.2, warp_distortion_horizontal: 0.0, warp_distortion_vertical: 0.0 },
        envelope_fidelity: 50.0,
        envelope_raster_mode: StudioEnvelopeRaster::ClippingMask,
        anti_alias: false,
        distort_appearance: false,
        distort_linear_gradient_fills: false,
        distort_pattern_fills: false,
    });
    let with_envelope = losses(&p);
    assert!(with_envelope.contains(&("export.envelope".into(), "envelope_not_exported")), "{with_envelope:?}");
}
