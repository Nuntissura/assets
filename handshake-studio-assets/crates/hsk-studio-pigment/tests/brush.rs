//! Brush engine proofs (lane SMB-PIGMENT-BRUSH). Hand-computed values.
use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};
use hsk_studio_folio::{ContentDigest, GeometryUnit, Length, TileRef, TileSchema};
use hsk_studio_observe::{DeliveryClass, DeliveryError, SinkPort, TileAddress};
use hsk_studio_pigment::brush::*;
use hsk_studio_pigment::caller::{MemoryAdmission, SerializedOwner};
use hsk_studio_pigment::render_plan::MaskFormat;
use hsk_studio_pigment::selection::{CombineMode, CoveragePlane, SelectionLimits};
use hsk_studio_pigment::*;
use hsk_studio_prism::{ProfileInput, profile_hash};

const LINEAR: &[u8] =
    include_bytes!("../../hsk-studio-prism/tests/profiles/sRGB-admitted-linear-v4.icc");

fn s(x: f64, y: f64, p: Option<f32>) -> StrokeSample {
    StrokeSample { x, y, pressure: p, time_ms: None }
}
fn lim() -> BrushLimits {
    BrushLimits::default()
}
fn xs(d: &[Dab]) -> Vec<f64> {
    d.iter().map(|d| d.x).collect()
}

#[test]
fn spacing_and_pressure_are_exact() {
    let dyn0 = BrushDynamics::default();
    let tip = BrushTip::round(20.0, 100.0, 25.0);
    let d = place_dabs(&tip, &dyn0, &[s(0.0, 0.0, None), s(100.0, 0.0, None)], &lim()).unwrap();
    assert_eq!(d.len(), 21);
    assert_eq!(xs(&d), (0..=20).map(|i| f64::from(i) * 5.0).collect::<Vec<_>>());
    // Spacing above 100 % is accepted: every 38 px.
    let wide = BrushTip::round(20.0, 100.0, 190.0);
    let d = place_dabs(&wide, &dyn0, &[s(0.0, 0.0, None), s(100.0, 0.0, None)], &lim()).unwrap();
    assert_eq!(xs(&d), vec![0.0, 38.0, 76.0]);
    // Two segments carry the remainder: 0..12 then up to (12, 13); step 5 -> 5, 10, then 15 is 3
    // into the second segment, 20 is 8 in.
    let d = place_dabs(&BrushTip::round(20.0, 100.0, 25.0), &dyn0, &[s(0.0, 0.0, None), s(12.0, 0.0, None), s(12.0, 13.0, None)], &lim()).unwrap();
    let pos: Vec<(f64, f64)> = d.iter().map(|d| (d.x, d.y)).collect();
    assert_eq!(pos, vec![(0.0, 0.0), (5.0, 0.0), (10.0, 0.0), (12.0, 3.0), (12.0, 8.0), (12.0, 13.0)]);
    // spacing_enabled false = one dab per sample; dab list is serde data.
    let mut one = tip;
    one.spacing_enabled = false;
    let d1 = place_dabs(&one, &dyn0, &[s(1.0, 1.0, None), s(9.0, 9.0, None)], &lim()).unwrap();
    assert_eq!(d1.len(), 2);
    let json = serde_json::to_string(&d).unwrap();
    assert_eq!(serde_json::from_str::<Vec<Dab>>(&json).unwrap(), d);
    // Pressure on size: minimum 20 %, pressure 0 -> 4, 1 -> 20, linear between samples.
    let pdyn = BrushDynamics {
        size: Channel { base_value: 100.0, minimum: 20.0, jitter: 0.0, control: Control::PenPressure },
        ..BrushDynamics::default()
    };
    let mut spaced = tip;
    spaced.spacing_enabled = false;
    let d = place_dabs(&spaced, &pdyn, &[s(0.0, 0.0, Some(0.0)), s(5.0, 0.0, Some(0.5)), s(9.0, 0.0, Some(1.0))], &lim()).unwrap();
    let dia: Vec<f64> = d.iter().map(|d| d.diameter).collect();
    assert_eq!(dia.len(), 3);
    assert!((dia[0] - 4.0).abs() < 1e-9 && (dia[1] - 12.0).abs() < 1e-9 && (dia[2] - 20.0).abs() < 1e-9, "{dia:?}");
    let mut tilt = pdyn;
    tilt.size.control = Control::PenTilt;
    assert_eq!(place_dabs(&tip, &tilt, &[s(0.0, 0.0, None)], &lim()).unwrap_err(), BrushError::DynamicsControlUnsupported);
    let mut sampled = tip;
    sampled.tip_kind = TipKind::Sampled;
    assert_eq!(place_dabs(&sampled, &dyn0, &[s(0.0, 0.0, None)], &lim()).unwrap_err(), BrushError::TipKindUnsupported);
}

fn dab(x: f64, y: f64, diameter: f64, opacity: f64, flow: f64) -> Dab {
    Dab { x, y, diameter, opacity, flow, angle: 0.0, roundness: 1.0 }
}
const CANVAS: Rect = Rect { x: 0, y: 0, width: 64, height: 64 };

#[test]
fn coverage_hardness_flow_opacity_and_selection_clip() {
    // Hard single dab: centre pixel full, far pixel zero.
    let p = stroke_coverage(&[dab(10.5, 10.5, 10.0, 1.0, 1.0)], 100.0, CANVAS, &lim()).unwrap();
    assert_eq!(p.at(10, 10), 65535);
    assert_eq!(p.at(30, 30), 0);
    // Flow 0.5 twice -> 0.75; opacity 0.5 caps at 0.375 (24576 = round(0.375 * 65535)).
    let two = [dab(10.5, 10.5, 10.0, 1.0, 0.5), dab(10.5, 10.5, 10.0, 1.0, 0.5)];
    assert_eq!(stroke_coverage(&two, 100.0, CANVAS, &lim()).unwrap().at(10, 10), 49151);
    let capped = [dab(10.5, 10.5, 10.0, 0.5, 0.5), dab(10.5, 10.5, 10.0, 0.5, 0.5)];
    assert_eq!(stroke_coverage(&capped, 100.0, CANVAS, &lim()).unwrap().at(10, 10), 24576);
    // Hardness profile, r = 10, pixel (18, 10): distance 8 -> d = 0.8.
    let big = [dab(10.5, 10.5, 20.0, 1.0, 1.0)];
    let at = |h| stroke_coverage(&big, h, CANVAS, &lim()).unwrap().at(18, 10);
    assert_eq!(at(100.0), 65535);
    assert!((i32::from(at(50.0)) - 23068).abs() <= 1, "{}", at(50.0)); // t = 0.4 -> 0.352
    assert!((i32::from(at(0.0)) - 6816).abs() <= 1, "{}", at(0.0)); // t = 0.2 -> 0.104
    assert!(at(0.0) < at(50.0) && at(50.0) < at(100.0));
    // Selection clip: a selection covering only x >= 10 removes the left half.
    let stroke = stroke_coverage(&[dab(10.0, 10.0, 8.0, 1.0, 1.0)], 100.0, CANVAS, &lim()).unwrap();
    let sel = CoveragePlane::filled(Rect { x: 10, y: 0, width: 54, height: 64 }, 65535, &SelectionLimits::default()).unwrap();
    let clipped = clip_to_selection(&stroke, &sel, &lim()).unwrap();
    assert_eq!(clipped.at(8, 10), 0);
    assert_eq!(clipped.at(11, 10), stroke.at(11, 10));
    assert!(stroke.at(8, 10) > 0);
    // paint_mask: diminish removes the stroke from a mask.
    let mask = CoveragePlane::filled(CANVAS, 65535, &SelectionLimits::default()).unwrap();
    let m = paint_mask(Some(&mask), &stroke, CombineMode::Diminish, &lim()).unwrap();
    assert_eq!(m.at(10, 10), 0);
    assert_eq!(m.at(40, 40), 65535);
}

#[derive(Default)]
struct Sink;
impl SinkPort for Sink {
    fn try_send(&mut self, class: DeliveryClass, _bytes: &[u8]) -> Result<(), DeliveryError> {
        assert_eq!(class, DeliveryClass::Terminal);
        Ok(())
    }
}

const T: u32 = 8;
const TILE: Rect = Rect { x: 0, y: 0, width: T, height: T };

struct Fx {
    actor: ActorContext,
    document: DomainId,
    layer: DomainId,
    profile: DomainId,
    tile: TileRef,
    colour: Vec<u8>,
    alpha: Vec<u8>,
}
impl Fx {
    fn new() -> Self {
        let actor = ActorContext::new("a", "p", "oa", "op", "s", "session").unwrap();
        let document = DomainId::parse("SDOC-019abcde-0000-7000-8000-000000000001").unwrap();
        let layer = DomainId::parse("SLYR-019abcde-0000-7000-8000-000000000002").unwrap();
        let profile = DomainId::parse("SCPF-019abcde-0000-7000-8000-000000000003").unwrap();
        let len = Length { value: f64::from(T), unit: GeometryUnit::Px };
        let tile = TileRef {
            object_key: "tile_0".into(),
            layer_id: layer.as_str().into(),
            column: 0,
            row: 0,
            width: len.clone(),
            height: len.clone(),
            format: "rgb_f32le".into(),
            colour_profile_id: profile.as_str().into(),
            schema_id: TileSchema::V1,
            content_digest: ContentDigest { algorithm: "sha256".into(), digest: "x".into() },
            artifact_manifest_id: "caller-artifact".into(),
        };
        let n = (T * T) as usize;
        let colour = (0..n).flat_map(|_| [0.2f32, 0.4, 0.6]).flat_map(f32::to_le_bytes).collect();
        let alpha = (0..n).flat_map(|_| 0.5f32.to_le_bytes()).collect();
        Self { actor, document, layer, profile, tile, colour, alpha }
    }
    fn resolved<'a>(&'a self, c: &'a [u8], a: &'a [u8]) -> ResolvedTile<'a> {
        ResolvedTile {
            tile_ref: &self.tile,
            bounds: TILE,
            sample_format: "rgb_f32le",
            colour_bytes: c,
            colour_stride_bytes: u64::from(T) * 12,
            colour_sha256: profile_hash(c),
            alpha_bytes: a,
            alpha_stride_bytes: u64::from(T) * 4,
            alpha_sha256: profile_hash(a),
            transfer: "linear_light",
            alpha_association: "straight",
            profile: ProfileInput { profile_id: &self.profile, bytes: LINEAR, expected_sha256: profile_hash(LINEAR) },
        }
    }
    fn run(&self, mask: &[u8], rc: &[u8], ra: &[u8]) -> (Vec<u8>, Vec<u8>, Vec<u8>, Vec<u8>) {
        let items = [TileItem { before: self.resolved(&self.colour, &self.alpha), replacement: self.resolved(rc, ra), expected_property_revision: 7 }];
        let r = Request {
            transport_version: 1,
            operation: "masked_replace",
            document_id: &self.document,
            layer_id: &self.layer,
            command_id: "brush-stroke",
            correlation_id: 1,
            actor: &self.actor,
            base_revision: 200,
            cancel_epoch: 4,
            grid: Grid { origin_x: 0, origin_y: 0, tile_width: T, tile_height: T },
            rectangle: TILE,
            mask: Mask { bounds: TILE, sample_format: "coverage_u16le", bytes: mask, stride_bytes: u64::from(T) * 2, sha256: profile_hash(mask) },
            tiles: &items,
            byte_limit: 1024 * 1024,
        };
        let mut owner = SerializedOwner::new(
            self.document.clone(),
            self.layer.clone(),
            self.actor.clone(),
            4,
            vec![(TileAddress::new(self.layer.clone(), "tile_0", 0, 0).unwrap(), 7)],
            MemoryAdmission::new(1024 * 1024).unwrap(),
            Sink,
        )
        .unwrap();
        let Outcome::Accepted(p) = masked_replace(&r, &mut owner, &CancellationToken::default()) else {
            panic!("expected accepted")
        };
        let u = &p.updates[0];
        (u.after.colour.to_vec(), u.after.alpha.to_vec(), u.preimage.colour.to_vec(), u.preimage.alpha.to_vec())
    }
}

#[test]
fn paint_and_erase_go_through_masked_replace() {
    let f = Fx::new();
    let stroke = place_dabs(&BrushTip::round(4.0, 100.0, 25.0), &BrushDynamics::default(), &[s(2.5, 4.5, None), s(5.5, 4.5, None)], &lim()).unwrap();
    let cov = stroke_coverage(&stroke, 100.0, CANVAS, &lim()).unwrap();
    let mask_plane = crop_to(&cov, TILE, &lim()).unwrap();
    let mask = mask_plane.to_mask_bytes(MaskFormat::CoverageU16Le).unwrap();
    let px = |x: usize, y: usize| y * T as usize + x;
    // Paint red.
    let (rc, ra) = paint_replacement(TILE, [0.8, 0.1, 0.1], &lim()).unwrap();
    let (c, a, pc, pa) = f.run(&mask, &rc, &ra);
    assert_eq!((pc, pa), (f.colour.clone(), f.alpha.clone()));
    let (mut inside, mut outside) = (0, 0);
    for y in 0..T as usize {
        for x in 0..T as usize {
            let m = mask_plane.data[px(x, y)];
            let (o, e) = (px(x, y) * 12, px(x, y) * 4);
            if m == 0 {
                assert_eq!(&c[o..o + 12], &f.colour[o..o + 12]);
                assert_eq!(&a[e..e + 4], &f.alpha[e..e + 4]);
                outside += 1;
            } else {
                let cov = f64::from(m) / 65535.0;
                let want = ((1.0 - cov) * 0.5 + cov) as f32;
                let got = f32::from_le_bytes(a[e..e + 4].try_into().unwrap());
                assert!((got - want).abs() < 1e-6, "({x},{y}) {got} vs {want}");
                inside += 1;
            }
        }
    }
    assert!(inside > 0 && outside > 0);
    // Erase: colour kept, alpha lowered by exactly the coverage.
    let (ec, ea) = erase_replacement(&f.colour, TILE, &lim()).unwrap();
    let route = route_erase(false, &mask_plane, None, &lim()).unwrap();
    assert_eq!(route, EraseRoute::Pixels);
    let (c, a, _, _) = f.run(&mask, &ec, &ea);
    for i in 0..(T * T) as usize {
        let cov = f64::from(mask_plane.data[i]) / 65535.0;
        let got = f32::from_le_bytes(a[i * 4..i * 4 + 4].try_into().unwrap());
        assert!((f64::from(got) - (1.0 - cov) * 0.5).abs() < 1e-6);
        if got > 0.0 {
            assert_eq!(&c[i * 12..i * 12 + 12], &f.colour[i * 12..i * 12 + 12]);
        }
    }
    // Non-pixel-editable target: the stroke goes to the layer mask, pixels are not requested.
    let EraseRoute::Mask { plane, receipt } = route_erase(true, &cov, None, &lim()).unwrap() else {
        panic!("expected mask route")
    };
    assert_eq!(receipt, "erase_routed_to_mask");
    assert_eq!(plane.at(5, 4), 65535 - cov.at(5, 4));
}

#[test]
fn hostile_sizes_and_values() {
    // dims_u32_max: a 5000 px dab on a u32::MAX canvas exceeds the byte budget before allocating.
    let huge = Rect { x: 0, y: 0, width: u32::MAX, height: u32::MAX };
    assert_eq!(stroke_coverage(&[dab(2500.0, 2500.0, 5000.0, 1.0, 1.0)], 100.0, huge, &lim()).unwrap_err(), BrushError::Pigment(Error::BudgetExceeded));
    assert!(crop_to(&CoveragePlane { bounds: CANVAS, data: vec![] }, huge, &lim()).is_err());
    assert!(matches!(stroke_coverage(&[dab(1.0e300, 0.0, 4.0, 1.0, 1.0)], 100.0, huge, &lim()).unwrap_err(), BrushError::Pigment(Error::Overflow)));
    // aggregate_over_budget: diameter 1, spacing 1 % over 10^6 px = 10^8 dabs, refused before placing.
    let tip = BrushTip::round(1.0, 100.0, 1.0);
    let line = [s(0.0, 0.0, None), s(1.0e6, 0.0, None)];
    assert_eq!(place_dabs(&tip, &BrushDynamics::default(), &line, &lim()).unwrap_err(), BrushError::Pigment(Error::BudgetExceeded));
    let tiny = BrushLimits { max_dabs: 3, ..lim() };
    assert_eq!(place_dabs(&BrushTip::round(20.0, 100.0, 25.0), &BrushDynamics::default(), &[s(0.0, 0.0, None), s(100.0, 0.0, None)], &tiny).unwrap_err(), BrushError::Pigment(Error::BudgetExceeded));
    // Non-finite and out-of-range inputs; no clamping.
    let d0 = BrushDynamics::default();
    assert_eq!(place_dabs(&tip, &d0, &[s(f64::NAN, 0.0, None)], &lim()).unwrap_err(), BrushError::Pigment(Error::Nonfinite));
    assert_eq!(place_dabs(&tip, &d0, &[s(0.0, 0.0, Some(f32::INFINITY))], &lim()).unwrap_err(), BrushError::Pigment(Error::Nonfinite));
    assert_eq!(place_dabs(&tip, &d0, &[s(0.0, 0.0, Some(1.5))], &lim()).unwrap_err(), BrushError::OutOfRange);
    assert_eq!(place_dabs(&BrushTip::round(5001.0, 50.0, 25.0), &d0, &[s(0.0, 0.0, None)], &lim()).unwrap_err(), BrushError::OutOfRange);
    assert_eq!(place_dabs(&BrushTip::round(10.0, 101.0, 25.0), &d0, &[s(0.0, 0.0, None)], &lim()).unwrap_err(), BrushError::OutOfRange);
    assert_eq!(stroke_coverage(&[dab(1.0, 1.0, 4.0, 1.0, 1.0)], 101.0, CANVAS, &lim()).unwrap_err(), BrushError::OutOfRange);
}
