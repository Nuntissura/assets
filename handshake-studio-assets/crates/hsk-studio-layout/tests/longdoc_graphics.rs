//! SMB-LAYOUT-LONGDOC slice 2: placed graphics and fitting. Hand-computed oracles: frame 100 x 50
//! at (10, 20), content 200 x 50 pt at 100 %.
use hsk_studio_folio::*;
use hsk_studio_layout::*;

const FRAME: Rect = Rect { x0_pt: 10.0, y0_pt: 20.0, x1_pt: 110.0, y1_pt: 70.0 };
const NO_CROP: Crop = Crop { top: 0.0, left: 0.0, bottom: 0.0, right: 0.0 };

fn fit(f: Option<StudioFit>, anchor: StudioFitAnchor, crop: Crop) -> Result<Fitted, FitError> {
    fit_content(FRAME, 200.0, 50.0, f, Some(StudioFit::Proportionally), anchor, crop)
}
fn t(f: Option<StudioFit>, anchor: StudioFitAnchor) -> [f64; 6] {
    fit(f, anchor, NO_CROP).unwrap().transform
}

/// proportionally: s = min(100/200, 50/50) = 0.5, box 100 x 25 centred -> (10, 32.5);
/// fill_proportionally: s = 1, box 200 x 50 centred -> x 10 - 50 = -40; content_to_frame: 0.5 x 1;
/// center_content: 100 % centred -> (-40, 20); preserve_sizes at top-right -> x 10 - 100 = -90;
/// frame_to_content at bottom-right keeps the frame's bottom-right corner: frame (-90, 20)-(110, 70);
/// cleared fitting: 100 % at the frame origin; crop 50 left + 50 right leaves 100 x 50, fitted
/// proportionally at 100 %: content x 50 lands on the frame's left edge (tx = 10 - 50);
/// apply_frame_fitting_options re-applies the stored rule (proportionally here).
#[test]
fn fitting_modes_hand_computed() {
    use StudioFit::*;
    use StudioFitAnchor::*;
    assert_eq!(t(Some(Proportionally), Center), [0.5, 0.0, 0.0, 0.5, 10.0, 32.5]);
    let r = fit(Some(Proportionally), Center, NO_CROP).unwrap().content_box;
    assert_eq!((r.x0_pt, r.y0_pt, r.x1_pt, r.y1_pt), (10.0, 32.5, 110.0, 57.5));
    assert_eq!(t(Some(FillProportionally), Center), [1.0, 0.0, 0.0, 1.0, -40.0, 20.0]);
    assert_eq!(t(Some(ContentToFrame), BottomRight), [0.5, 0.0, 0.0, 1.0, 10.0, 20.0]);
    assert_eq!(t(Some(CenterContent), TopLeft), [1.0, 0.0, 0.0, 1.0, -40.0, 20.0]);
    assert_eq!(t(Some(PreserveSizes), TopRight), [1.0, 0.0, 0.0, 1.0, -90.0, 20.0]);
    let f = fit(Some(FrameToContent), BottomRight, NO_CROP).unwrap();
    assert_eq!((f.frame.x0_pt, f.frame.y0_pt, f.frame.x1_pt, f.frame.y1_pt), (-90.0, 20.0, 110.0, 70.0));
    assert_eq!(f.transform, [1.0, 0.0, 0.0, 1.0, -90.0, 20.0]);
    assert_eq!(t(None, BottomRight), [1.0, 0.0, 0.0, 1.0, 10.0, 20.0]);
    let crop = Crop { top: 0.0, left: 50.0, bottom: 0.0, right: 50.0 };
    assert_eq!(fit(Some(Proportionally), Center, crop).unwrap().transform, [1.0, 0.0, 0.0, 1.0, -40.0, 20.0]);
    assert_eq!(t(Some(ApplyFrameFittingOptions), Center), t(Some(Proportionally), Center));
}

/// Typed refusals: content-aware fitting is unavailable in this build (STU-LAY-131 typed channel),
/// honour_existing_style is data-merge only, apply without a stored rule, non-finite or empty
/// content, a crop that eats the content, a scale that overflows. A Folio graphic frame on the
/// second page of a spread lands in spread coordinates (page 1 is 100 pt wide).
#[test]
fn fitting_refusals_and_folio_frame_on_its_spread() {
    use StudioFit::*;
    assert_eq!(fit(Some(ContentAware), StudioFitAnchor::Center, NO_CROP), Err(FitError::ContentAwareUnavailable));
    assert_eq!(fit(Some(HonourExistingStyle), StudioFitAnchor::Center, NO_CROP), Err(FitError::NotLegalHere));
    let none = fit_content(FRAME, 200.0, 50.0, Some(ApplyFrameFittingOptions), None, StudioFitAnchor::Center, NO_CROP);
    assert_eq!(none, Err(FitError::NoStoredFitting));
    let bad = Err(FitError::Layout(LayoutError::InvalidGeometry));
    assert_eq!(fit_content(FRAME, f64::NAN, 50.0, Some(Proportionally), None, StudioFitAnchor::Center, NO_CROP), bad);
    assert_eq!(fit_content(FRAME, 0.0, 50.0, Some(Proportionally), None, StudioFitAnchor::Center, NO_CROP), bad);
    let eat = Crop { top: 0.0, left: 120.0, bottom: 0.0, right: 80.0 };
    assert_eq!(fit(Some(Proportionally), StudioFitAnchor::Center, eat), bad);
    assert_eq!(fit_content(FRAME, 1e-320, 1e-320, Some(ContentToFrame), None, StudioFitAnchor::Center, NO_CROP), bad);

    let len = |v: f64| Length { value: v, unit: GeometryUnit::Pt };
    let edges = LayoutEdges { top: len(0.0), right: len(0.0), bottom: len(0.0), left: len(0.0) };
    let page = |k: &str| StudioPage { page_id: k.into(), width: len(100.0), height: len(100.0), side: PageSide::SingleSided, margins: edges.clone(), bleed: None };
    let doc = StudioDocument {
        schema_id: DocumentSchema::V1,
        document_id: "SDOC-019abcde-0000-7000-8000-000000000001".into(),
        revision: 1,
        geometry_unit: GeometryUnit::Pt,
        colour_mode: 3,
        bits_per_channel: 8,
        working_space_rgb: ProfileBinding::Unassigned(()),
        working_space_cmyk: ProfileBinding::Unassigned(()),
        working_space_gray: ProfileBinding::Unassigned(()),
        working_space_spot: ProfileBinding::Unassigned(()),
        blending_space: ProfileBinding::Unassigned(()),
        artboards: vec![],
        page_spreads: vec![StudioPageSpread {
            schema_id: PageSpreadSchema::V1,
            page_spread_id: "SPGS-019abcde-0000-7000-8000-000000000001".into(),
            name: "s".into(),
            parent: Parent::Root(()),
            bounds: Bounds { x: len(0.0), y: len(0.0), width: len(200.0), height: len(100.0) },
            pages: vec![page("p1"), page("p2")],
        }],
        layers: vec![],
        graph: StudioLayerGraph { schema_id: GraphSchema::V1, operations: vec![], edges: vec![], outputs: vec![] },
        sequences: vec![],
    };
    let g = StudioGraphicFrame {
        layer_id: "SLYR-019abcde-0000-7000-8000-000000000003".into(),
        page_id: "p2".into(),
        bounds: Bounds { x: len(10.0), y: len(20.0), width: len(80.0), height: len(40.0) },
        shape: StudioFrameShape::Ellipse,
        content_width: len(40.0),
        content_height: len(40.0),
        fit: Some(Proportionally),
        frame_fitting: None,
        anchor: StudioFitAnchor::TopLeft,
        crop: None,
    };
    let p = place_graphic(&doc, &g).unwrap();
    // Frame (110, 20)-(190, 60); s = min(80/40, 40/40) = 1, top-left anchor.
    assert_eq!((p.spread, p.page, p.frame.x0_pt, p.frame.x1_pt), (0, 1, 110.0, 190.0));
    assert_eq!(p.transform, [1.0, 0.0, 0.0, 1.0, 110.0, 20.0]);
    assert_eq!(p.shape, StudioFrameShape::Ellipse);
    let mut lost = g.clone();
    lost.page_id = "p9".into();
    assert_eq!(place_graphic(&doc, &lost), Err(FitError::Layout(LayoutError::InvalidInput)));
}
