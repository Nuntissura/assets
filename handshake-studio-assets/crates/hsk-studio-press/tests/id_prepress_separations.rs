//! Lane SMB-ID-PREPRESS slice 2: ink manager, mixed inks and the separations preview (STU-LAY-160,
//! STU-LAY-250, STU-COL-212). Every expected value is hand-computed from pixel-aligned rectangles
//! at 1 px/pt on a 100 x 100 pt page (a rectangle covers its pixels fully). InDesign/Acrobat parity
//! of the preview is UNVERIFIED (D5).
use hsk_studio_accord::CancellationToken;
use hsk_studio_press::*;

const SPOT: &str = "PANTONE 185 C";
const BOX: [f64; 4] = [0.0, 0.0, 100.0, 100.0];

fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<VectorEl> {
    vec![VectorEl::MoveTo([x0, y0]), VectorEl::LineTo([x1, y0]), VectorEl::LineTo([x1, y1]), VectorEl::LineTo([x0, y1]), VectorEl::Close]
}
const FILL: VectorMode = VectorMode::Fill { even_odd: false };

fn draw(s: &mut Separator<'_>, r: &[VectorEl], paint: SepPaint, overprint: bool) {
    s.draw(&SepDraw { path: r, mode: &FILL, paint, overprint, overprint_nonzero: false }).unwrap();
}

fn at(p: &SeparationPreview, x: f64, y: f64) -> Vec<(String, f64)> {
    p.sample_pt(x, y).unwrap().into_iter().map(|r| (r.ink, r.percent)).collect()
}

fn reading(c: f64, m: f64, y: f64, k: f64, spot: Option<f64>) -> Vec<(String, f64)> {
    let mut v = vec![
        ("Process Cyan".to_string(), c),
        ("Process Magenta".to_string(), m),
        ("Process Yellow".to_string(), y),
        ("Process Black".to_string(), k),
    ];
    if let Some(s) = spot {
        v.push((SPOT.to_string(), s));
    }
    v
}

/// C 50 % over (10..60)^2, then the spot at 100 % over (40..90)^2.
fn two_rects(inks: &InkManager, overprint: bool) -> SeparationPreview {
    let mut s = Separator::new(BOX, 1.0, inks, SepLimits::default()).unwrap();
    draw(&mut s, &rect(10.0, 10.0, 60.0, 60.0), SepPaint::Process([0.5, 0.0, 0.0, 0.0]), false);
    draw(&mut s, &rect(40.0, 40.0, 90.0, 90.0), SepPaint::Spot { name: SPOT.into(), tint: 1.0 }, overprint);
    s.finish()
}

#[test]
fn separations_knockout_overprint_readouts_and_views() {
    let inks = InkManager::default();
    let ko = two_rects(&inks, false);
    assert_eq!((ko.width, ko.height), (100, 100));
    assert_eq!(ko.plates.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["Process Cyan", "Process Magenta", "Process Yellow", "Process Black", SPOT]);
    assert_eq!(at(&ko, 20.0, 20.0), reading(50.0, 0.0, 0.0, 0.0, Some(0.0)));
    // Knockout: the spot removes cyan under it.
    assert_eq!(at(&ko, 50.0, 50.0), reading(0.0, 0.0, 0.0, 0.0, Some(100.0)));
    assert_eq!(ko.total_pt(50.0, 50.0), Some(100.0));
    // Cyan covers 50 x 50 minus the 20 x 20 overlap = 2100 pixels; mean 50 % x 2100 / 10000.
    let cov = ko.coverage();
    assert_eq!((cov[0].inked_pixels, cov[0].max_percent, cov[0].mean_percent), (2100, 50.0, 10.5));
    assert_eq!((cov[4].inked_pixels, cov[4].max_percent, cov[4].mean_percent), (2500, 100.0, 25.0));
    // Overprint: cyan stays under the spot.
    let op = two_rects(&inks, true);
    assert_eq!(at(&op, 50.0, 50.0), reading(50.0, 0.0, 0.0, 0.0, Some(100.0)));
    assert_eq!(op.total_pt(50.0, 50.0), Some(150.0));
    assert_eq!(op.coverage()[0].inked_pixels, 2500);
    // Views: pixel (20, 79) = page point (20.5, 20.5) holds C 50 %.
    let px = |rgb: &[u8], x: usize, y: usize| [rgb[(y * 100 + x) * 3], rgb[(y * 100 + x) * 3 + 1], rgb[(y * 100 + x) * 3 + 2]];
    let cyan_only = op.render(&PlateView::Composite(vec![true, false, false, false, false]));
    assert_eq!(px(&cyan_only, 20, 79), [128, 255, 255]);
    assert_eq!(px(&cyan_only, 50, 50), [128, 255, 255]);
    let spot_plate = op.render(&PlateView::Single(4));
    assert_eq!((px(&spot_plate, 50, 50), px(&spot_plate, 20, 79)), ([0, 0, 0], [255, 255, 255]));
    let limit = op.render(&PlateView::InkLimit(120.0));
    assert_eq!(px(&limit, 50, 50), [255, 0, 0]);
    assert_ne!(px(&limit, 20, 79), [255, 0, 0]);
    // Outside the canvas there is no reading.
    assert_eq!(op.sample_pt(100.0, 50.0), None);
}

#[test]
fn ink_limits_are_three_independent_limits_scaled_by_ink_count() {
    let inks = InkManager::default();
    let mut s = Separator::new(BOX, 1.0, &inks, SepLimits::default()).unwrap();
    draw(&mut s, &rect(10.0, 10.0, 60.0, 60.0), SepPaint::Process([0.5, 0.0, 0.0, 0.0]), false);
    draw(&mut s, &rect(40.0, 40.0, 90.0, 90.0), SepPaint::Spot { name: SPOT.into(), tint: 1.0 }, true);
    // 400 % rich black over page (0..10)^2 = device rows 90..99, columns 0..9.
    draw(&mut s, &rect(0.0, 0.0, 10.0, 10.0), SepPaint::Process([1.0, 1.0, 1.0, 1.0]), false);
    let p = s.finish();
    let total = p.check_ink_limits(&InkLimits { total_ink_limit: Some(300.0), coverage_warning_margin: Some(200.0), ..Default::default() }).unwrap();
    assert_eq!((total.structural_max_percent, total.max_total_percent), (500.0, 400.0));
    let f: Vec<(InkLimitSeverity, u64, f64, (u32, u32))> = total.findings.iter().map(|f| (f.severity, f.pixels, f.max_percent, f.first_px)).collect();
    // Error: the 100 rich-black pixels; warning (100, 300]: the 400 overlap pixels at 150 %.
    assert_eq!(f, vec![(InkLimitSeverity::Error, 100, 400.0, (0, 90)), (InkLimitSeverity::Warning, 400, 150.0, (40, 40))]);
    // Single-ink and black limits are separate fields with separate findings.
    let single = p.check_ink_limits(&InkLimits { single_ink_limit: Some(95.0), black_ink_limit: Some(90.0), ..Default::default() }).unwrap();
    let kinds: Vec<(LimitKind, u64)> = single.findings.iter().map(|f| (f.limit.clone(), f.pixels)).collect();
    assert_eq!(
        kinds,
        vec![
            (LimitKind::Single("Process Cyan".into()), 100),
            (LimitKind::Single("Process Magenta".into()), 100),
            (LimitKind::Single("Process Yellow".into()), 100),
            (LimitKind::Single("Process Black".into()), 100),
            (LimitKind::Single(SPOT.into()), 2500),
            (LimitKind::Black, 100),
        ]
    );
    // The total bound scales with the ink count (5 inks: 500 %), not clamped at 400.
    assert!(p.check_ink_limits(&InkLimits { total_ink_limit: Some(450.0), ..Default::default() }).unwrap().findings.is_empty());
    assert_eq!(p.check_ink_limits(&InkLimits { total_ink_limit: Some(501.0), ..Default::default() }), Err(SepError::InvalidInput));
    assert_eq!(p.check_ink_limits(&InkLimits { black_ink_limit: Some(f64::NAN), ..Default::default() }), Err(SepError::InvalidInput));
}

#[test]
fn ink_manager_aliases_conversion_and_mixed_inks() {
    let mut inks = InkManager::default();
    let mut a = Ink::spot("Spot A", None);
    a.alias = Some("Spot B".into());
    let b = Ink::spot("Spot B", Some([0.0, 0.91, 0.76, 0.0]));
    let mut c = Ink::spot("Spot C", Some([0.2, 0.0, 0.0, 0.0]));
    c.convert_to_process = true;
    inks.inks.extend([a, b, c]);
    inks.validate().unwrap();
    let mut s = Separator::new(BOX, 1.0, &inks, SepLimits::default()).unwrap();
    draw(&mut s, &rect(0.0, 0.0, 10.0, 10.0), SepPaint::Spot { name: "Spot A".into(), tint: 0.5 }, false);
    draw(&mut s, &rect(20.0, 0.0, 30.0, 10.0), SepPaint::Spot { name: "Spot C".into(), tint: 0.5 }, false);
    let mixed = MixedInk { name: "Mix".into(), parts: vec![("Process Cyan".into(), 0.2), ("Spot B".into(), 0.5)] };
    draw(&mut s, &rect(40.0, 0.0, 50.0, 10.0), SepPaint::Mixed { ink: mixed, tint: 1.0 }, false);
    let p = s.finish();
    let named = |x: f64| -> Vec<(String, f64)> { at(&p, x, 5.0).into_iter().filter(|(_, v)| *v > 0.0).map(|(n, v)| (n, (v * 1000.0).round() / 1000.0)).collect() };
    // Aliased onto Spot B's plate; converted through the alternate; mixed parts on their plates.
    assert_eq!(named(5.0), vec![("Spot B".to_string(), 50.0)]);
    assert_eq!(named(25.0), vec![("Process Cyan".to_string(), 10.0)]);
    assert_eq!(named(45.0), vec![("Process Cyan".to_string(), 20.0), ("Spot B".to_string(), 50.0)]);
    assert!(p.plates.iter().all(|pl| pl.name != "Spot A" && pl.name != "Spot C"));
    // Captured typed errors.
    let mut bad = InkManager::default();
    bad.inks[0].alias = Some("Process Black".into());
    assert_eq!(bad.validate().unwrap_err().code(), "process_ink_cannot_be_aliased");
    let mut bad = InkManager::default();
    bad.inks[0].angle_deg = Some(361.0);
    assert_eq!(bad.validate().unwrap_err().code(), "ink_angle_out_of_range");
    let mut cyc = InkManager::default();
    let (mut x, mut y) = (Ink::spot("X", None), Ink::spot("Y", None));
    x.alias = Some("Y".into());
    y.alias = Some("X".into());
    cyc.inks.extend([x, y]);
    assert_eq!(cyc.validate().unwrap_err().code(), "ink_alias_cycle");
    let mut many = InkManager::default();
    many.inks.extend((0..32).map(|i| Ink::spot(&format!("S{i}"), None)));
    let spots = |n: usize| MixedInk { name: "M".into(), parts: (0..n).map(|i| (format!("S{i}"), 0.5)).collect() };
    assert!(spots(27).validate(&many).is_ok());
    assert_eq!(spots(28).validate(&many).unwrap_err().code(), "mixed_ink_too_many_spot_inks");
    let mut over = spots(27);
    over.parts.extend(ProcessInk::ALL.iter().map(|p| (p.name().to_string(), 0.1)));
    assert!(over.validate(&many).is_ok());
    over.parts.push(("S27".into(), 0.1));
    assert_eq!(over.validate(&many).unwrap_err().code(), "mixed_ink_too_many_inks");
    // Mixed-ink group: 3 x 2 = 6 swatches, first row slowest.
    let row = |ink: &str, initial: f64, repeat: u32, increment: f64| MixedInkGroupRow { ink: ink.into(), initial, repeat, increment };
    let g = MixedInkGroup { name: "G".into(), rows: vec![row("S0", 0.1, 2, 0.1), row("S1", 0.5, 1, 0.25)] };
    let made = g.generate(&many).unwrap();
    assert_eq!(made.len(), 6);
    assert_eq!((made[1].name.as_str(), made[1].parts.clone()), ("G 2", vec![("S0".to_string(), 0.1), ("S1".to_string(), 0.75)]));
    assert!((made[5].parts[0].1 - 0.3).abs() < 1e-12 && made[5].parts[1].1 == 0.75);
    let big = |reps: &[u32]| MixedInkGroup { name: "B".into(), rows: reps.iter().map(|r| row("S0", 0.0, *r, 0.0)).collect() };
    assert_eq!(big(&[9, 9, 9]).swatch_count(), Ok(1000));
    assert_eq!(big(&[9, 9, 9, 1]).swatch_count().unwrap_err().code(), "mixed_ink_group_too_large");
    assert_eq!(big(&[u32::MAX, u32::MAX]).swatch_count().unwrap_err().code(), "mixed_ink_group_too_large");
    assert_eq!(MixedInkGroup { name: "T".into(), rows: vec![row("S0", 0.9, 2, 0.1)] }.generate(&many).unwrap_err().code(), "tint_out_of_range");
}

#[test]
fn separations_hostile_sizes_and_budgets() {
    let inks = InkManager::default();
    // dims_u32_max: refused before any plane is allocated.
    assert_eq!(Separator::new([0.0, 0.0, 1e7, 1e7], 1.0, &inks, SepLimits::default()).err(), Some(SepError::CanvasTooLarge));
    assert_eq!(Separator::new([0.0, 0.0, f64::INFINITY, 10.0], 1.0, &inks, SepLimits::default()).err(), Some(SepError::InvalidInput));
    assert_eq!(Separator::new(BOX, f64::NAN, &inks, SepLimits::default()).err(), Some(SepError::InvalidInput));
    // aggregate_over_budget: four process planes fit (4 x 40000 bytes), a spot plane does not.
    let tight = SepLimits { max_bytes: 5 * 40_000 - 1, ..SepLimits::default() };
    let mut s = Separator::new(BOX, 1.0, &inks, tight).unwrap();
    let r = rect(0.0, 0.0, 10.0, 10.0);
    assert_eq!(s.draw(&SepDraw { path: &r, mode: &FILL, paint: SepPaint::Spot { name: "S".into(), tint: 1.0 }, overprint: false, overprint_nonzero: false }), Err(SepError::Budget));
    // A clip mask is a plane too.
    assert_eq!(s.push_clip(&r, false), Err(SepError::Budget));
    // Plate count cap.
    let mut s = Separator::new([0.0, 0.0, 2.0, 2.0], 1.0, &inks, SepLimits::default()).unwrap();
    for i in 0..(MAX_SEP_PLATES - 4) {
        draw(&mut s, &r, SepPaint::Spot { name: format!("S{i}"), tint: 1.0 }, false);
    }
    assert_eq!(s.draw(&SepDraw { path: &r, mode: &FILL, paint: SepPaint::Spot { name: "one more".into(), tint: 1.0 }, overprint: false, overprint_nonzero: false }), Err(SepError::TooManyPlates));
    // Clip nesting cap, segment cap.
    let mut s = Separator::new(BOX, 1.0, &inks, SepLimits::default()).unwrap();
    for _ in 0..MAX_SEP_CLIP_DEPTH {
        s.push_clip(&r, false).unwrap();
    }
    assert_eq!(s.push_clip(&r, false), Err(SepError::ClipTooDeep));
    let mut s = Separator::new(BOX, 1.0, &inks, SepLimits { max_segments: 1, ..SepLimits::default() }).unwrap();
    assert_eq!(s.draw(&SepDraw { path: &r, mode: &FILL, paint: SepPaint::Process([1.0, 0.0, 0.0, 0.0]), overprint: false, overprint_nonzero: false }), Err(SepError::Budget));
    // Non-finite tints are clamped to no ink, never NaN planes.
    let mut s = Separator::new(BOX, 1.0, &inks, SepLimits::default()).unwrap();
    draw(&mut s, &r, SepPaint::Process([f64::NAN, 2.0, -1.0, 0.0]), false);
    assert_eq!(at(&s.finish(), 5.0, 5.0), reading(0.0, 100.0, 0.0, 0.0, None));
}

fn assemble(objects: &[Vec<u8>]) -> Vec<u8> {
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (i, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend(format!("{} 0 obj\n", i + 1).as_bytes());
        out.extend(body);
        out.extend(b"\nendobj\n");
    }
    let xref = out.len();
    out.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes());
    for o in offsets {
        out.extend(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objects.len() + 1).as_bytes());
    out
}

#[test]
fn pdf_page_separates_cmyk_gray_spot_and_clips() {
    let content = b"0.5 0 0 0 k 10 10 50 50 re f\n/Spot cs 1 scn 40 40 50 50 re f\n1 0 0 rg 0 0 5 5 re f\n0.2 g 80 80 10 10 re f\nq 60 0 40 30 re W n 0 0 0 1 k 0 0 100 100 re f Q\n";
    let mut stream = format!("<< /Length {} >>\nstream\n", content.len()).into_bytes();
    stream.extend(content);
    stream.extend(b"\nendstream");
    let pdf = assemble(&[
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Resources << /ColorSpace << /Spot [/Separation /PANTONE#20185#20C /DeviceCMYK << /FunctionType 2 /Domain [0 1] /C0 [0 0 0 0] /C1 [0 0.91 0.76 0] /N 1 >>] >> >> /Contents 4 0 R >>".to_vec(),
        stream,
    ]);
    let cancel = CancellationToken::default();
    let doc = read_vector_document(&pdf, 1 << 20, &cancel).unwrap();
    let p = separate_vector_page(&doc.pages[0], 1.0, &InkManager::default(), None, SepLimits::default(), &cancel).unwrap();
    assert_eq!(at(&p, 20.0, 20.0), reading(50.0, 0.0, 0.0, 0.0, Some(0.0)));
    assert_eq!(at(&p, 50.0, 50.0), reading(0.0, 0.0, 0.0, 0.0, Some(100.0)));
    // DeviceGray 0.2 -> K 80 %.
    let k = at(&p, 85.0, 85.0)[3].1;
    assert!((k - 80.0).abs() < 1e-4, "{k}");
    // The clipped black fill reaches only (60..100) x (0..30).
    assert_eq!(at(&p, 70.0, 10.0)[3].1, 100.0);
    assert_eq!(at(&p, 70.0, 50.0)[3].1, 0.0);
    // The RGB rectangle needs a colour engine: not separated, counted and noted.
    assert_eq!(p.unseparated, 1);
    assert!(p.notes.contains(&SepNote::Unseparated) && p.notes.contains(&SepNote::PdfOverprintNotRead));
    assert_eq!(at(&p, 2.0, 2.0).iter().map(|r| r.1).sum::<f64>(), 0.0);
    // A converter port separates it.
    struct Naive;
    impl ProcessConverter for Naive {
        fn to_cmyk(&self, space: &VectorSpace, c: &[f64]) -> Option<[f64; 4]> {
            (*space == VectorSpace::Rgb).then(|| [1.0 - c[0], 1.0 - c[1], 1.0 - c[2], 0.0])
        }
    }
    let q = separate_vector_page(&doc.pages[0], 1.0, &InkManager::default(), Some(&Naive), SepLimits::default(), &cancel).unwrap();
    assert_eq!((q.unseparated, at(&q, 2.0, 2.0)), (0, reading(0.0, 100.0, 100.0, 0.0, Some(0.0))));
    let canceled = CancellationToken::default();
    canceled.cancel();
    assert_eq!(separate_vector_page(&doc.pages[0], 1.0, &InkManager::default(), None, SepLimits::default(), &canceled).err(), Some(SepError::Canceled));
}
