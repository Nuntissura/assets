//! Lane SMB-PS-ADJUST-STYLES-2: smart filters over placed-asset instances. A placed layer's
//! `smart_filters` record converts through pigment's `SmartFilterOp::from_record` and wraps the
//! shared source scene in `SceneContent::Filtered` (the stack runs on the source surface, then the
//! instance transform places it, STU-CMP-010). Enabled masks, parameters pigment refuses and
//! hostile radii are typed refusals; a disabled stack is ignored. Pixel proof: render-cpu
//! `tests/cpu_placed_filters.rs`. Photoshop's document-space order is UNVERIFIED (D5).
use hsk_studio_composite::lower_extensions::placed::{
    PLACED_SMART_FILTER_MASK, PLACED_SMART_FILTER_PARAMETER, placed_scene_layer,
};
use hsk_studio_composite::{CompositeError, FilteredContent, SceneComposition, SceneContent};
use hsk_studio_folio::StudioRasterRecords;
use hsk_studio_pigment::blend_math::StudioBlendMode;
use hsk_studio_pigment::filter_math::LayerFilter;
use hsk_studio_pigment::smart_filter::StackFilter;
use hsk_studio_pigment::transform_math::ResampleFilter;
use serde_json::{Value, json};
use std::collections::BTreeMap;

fn identity(prefix: &str, n: u32) -> String {
    format!("{prefix}-019abcde-0000-7000-8000-{n:012x}")
}
fn records(stack: Value) -> StudioRasterRecords {
    serde_json::from_value(json!({"schema_id":"hsk.studio.raster_records@1","document_id":identity("SDOC",1),
        "layers":[{"layer_id":identity("SLYR",1),"placed_asset":{"source_id":"src1","transform":[1.0,0.0,0.0,1.0,3.0,0.0]},
            "smart_filters":stack}],
        "placed_sources":[{"source_id":"src1","location":{"mode":"embedded","document_id":identity("SDOC",2),
            "content_digest":"sha256-ab12"},"width":4.0,"height":2.0,"uses":[]}]}))
    .expect("records shape")
}
fn filter(id: &str, enabled: bool, params: Value, mask: Option<bool>) -> Value {
    let mut f = json!({"instance_id":id,"enabled":enabled,"opacity":100.0,"params":params});
    if let Some(on) = mask {
        f["mask"] = json!({"tiles":[],"enabled":on,"linked":true});
    }
    f
}
fn stack(enabled: bool, filters: Vec<Value>) -> Value {
    json!({"enabled":enabled,"filters":filters})
}
fn sources() -> BTreeMap<String, SceneComposition> {
    let scene = SceneComposition {
        composition_id: "src1".into(),
        width: 4,
        height: 2,
        background: Some([0.2, 0.4, 0.6, 1.0]),
        filter: ResampleFilter::NearestNeighbour,
        layers: Vec::new(),
    };
    BTreeMap::from([("src1".to_owned(), scene)])
}
fn content(stack: Value) -> Result<SceneContent, CompositeError> {
    let recs = records(stack);
    placed_scene_layer(&identity("SLYR", 1), &recs, &sources(), StudioBlendMode::Normal, 1.0, 1.0)
        .map(|l| l.expect("placed").content)
}

#[test]
fn placed_smart_filters_wrap_the_shared_source_and_refuse_typed() {
    let blur = json!({"filter_kind":"gaussian_blur","radius_px":1.5});
    let median = json!({"filter_kind":"median","radius_px":1});
    // blur enabled, median disabled, a DISABLED mask (applies everywhere: dropped)
    let got = content(stack(true, vec![filter("blur", true, blur.clone(), Some(false)), filter("median", false, median.clone(), None)])).unwrap();
    let SceneContent::Filtered(f) = got else { panic!("{got:?}") };
    let FilteredContent { content: inner, rect, filters } = *f;
    assert_eq!((rect.x, rect.y, rect.width, rect.height), (0, 0, 4, 2), "the stack runs over the source rect");
    assert!(matches!(inner, SceneContent::Precomp(ref s) if s.composition_id == "src1"), "{inner:?}");
    assert_eq!(filters.entries.len(), 2);
    assert!(matches!(filters.entries[0].filter, StackFilter::Filter(LayerFilter::GaussianBlur { radius_px }) if radius_px == 1.5));
    assert!(filters.entries[0].enabled && !filters.entries[1].enabled);
    assert!(filters.stack_mask.is_none() && filters.entries.iter().all(|e| e.mask.is_none()));
    // a disabled stack, or one without enabled entries, leaves the plain shared precomp
    assert!(matches!(content_of(stack(false, vec![filter("blur", true, blur.clone(), None)])), SceneContent::Precomp(_)));
    assert!(matches!(content_of(stack(true, vec![filter("median", false, median, None)])), SceneContent::Precomp(_)));
    // typed refusals: an enabled per-filter mask (document-space tiles, source-space stack) and a
    // hostile radius pigment refuses
    let refused = |s: Value, code: &str| match content(s) {
        Err(CompositeError::Unavailable(r)) => assert!(r.ends_with(code) && r.contains(&identity("SLYR", 1)), "{r}"),
        other => panic!("{code}: {other:?}"),
    };
    refused(stack(true, vec![filter("blur", true, blur.clone(), Some(true))]), PLACED_SMART_FILTER_MASK);
    refused(stack(true, vec![filter("blur", true, json!({"filter_kind":"gaussian_blur","radius_px":1.0e9}), None)]), PLACED_SMART_FILTER_PARAMETER);
}

fn content_of(stack: Value) -> SceneContent {
    content(stack).expect("placed content")
}
