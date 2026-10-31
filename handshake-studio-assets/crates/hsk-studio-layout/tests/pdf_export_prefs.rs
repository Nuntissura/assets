//! Lane SMB-ID-PDF-EXPORT-PREFS-1: the PDF export preference record of STU-LAY-158 (six panels,
//! nine numeric bound sets, partner-relative compression thresholds, requested vs effective
//! read-back, typed password error) and the STU-LAY-159 preset reference. Expectations are
//! hand-computed from the spec tables (14-studio-creative-suite.md :7093-7136) and from the Studio
//! readings stated in `src/pdf_export_prefs.rs` (InDesign parity UNVERIFIED, D5). Test names start
//! with the MT proof recipe filters (MT-30833..MT-30840).
use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};
use hsk_studio_layout::*;
use hsk_studio_observe::{Budget, DeliveryClass, DeliveryError, FailureCode, Observe, Outcome, SinkPort};

fn bounded_case(name: &'static str, body: impl FnOnce() + Send + 'static) {
    use std::{
        panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
        sync::mpsc,
        time::Duration,
    };
    let (sender, receiver) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let _ = sender.send(catch_unwind(AssertUnwindSafe(body)));
    });
    match receiver.recv_timeout(Duration::from_secs(30)) {
        Ok(Ok(())) => {}
        Ok(Err(p)) => resume_unwind(p),
        Err(_) => panic!("case {name} exceeded declared 30-second deadline"),
    }
}

struct Collect(Vec<Vec<u8>>);
impl SinkPort for Collect {
    fn try_send(&mut self, _: DeliveryClass, bytes: &[u8]) -> Result<(), DeliveryError> {
        self.0.push(bytes.to_vec());
        Ok(())
    }
}
fn observe() -> Observe {
    let actor = ActorContext::new("account", "principal", "owner", "owner-principal", "space", "session").unwrap();
    let resource = DomainId::parse("SDOC-019abcde-0000-7000-8000-000000000001").unwrap();
    Observe::new(1, 3, resource, actor, Budget::new(0, 0).unwrap())
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

fn token(t: &str) -> PdfToken {
    PdfToken::new(t).unwrap()
}

/// Parameter, name, hard_min, hard_max, unit code.
type AbsRow = (PdfParameter, &'static str, f64, f64, &'static str);
/// Threshold parameter, name, partner.
type RelRow = (PdfParameter, &'static str, PdfParameter);
/// Sets one bleed.
type SetBleed = fn(&mut MarksAndBleedsPanel, f64);

// ---- MT-30834 numeric bound sets; MT-30833 AC-001 relative bounds; STU-LAY-168 ----

#[test]
fn pdf_numeric_relative_bound_metadata() {
    bounded_case("pdf_numeric_relative_bound_metadata", || {
        let abs: [AbsRow; 6] = [
            (PdfParameter::ColorBitmapSamplingDpi, "color_bitmap_sampling_dpi", 9.0, 2400.0, "dpi"),
            (PdfParameter::GrayscaleBitmapSamplingDpi, "grayscale_bitmap_sampling_dpi", 9.0, 2400.0, "dpi"),
            (PdfParameter::MonochromeBitmapSamplingDpi, "monochrome_bitmap_sampling_dpi", 9.0, 2400.0, "dpi"),
            (PdfParameter::SubsetFontsBelow, "subset_fonts_below", 0.0, 100.0, "percent"),
            (PdfParameter::ColorTileSize, "color_tile_size", 128.0, 2048.0, "px"),
            (PdfParameter::GrayTileSize, "gray_tile_size", 128.0, 2048.0, "px"),
        ];
        let rel: [RelRow; 3] = [
            (PdfParameter::ThresholdToCompressColor, "threshold_to_compress_color", PdfParameter::ColorBitmapSamplingDpi),
            (PdfParameter::ThresholdToCompressGray, "threshold_to_compress_gray", PdfParameter::GrayscaleBitmapSamplingDpi),
            (
                PdfParameter::ThresholdToCompressMonochrome,
                "threshold_to_compress_monochrome",
                PdfParameter::MonochromeBitmapSamplingDpi,
            ),
        ];
        assert_eq!(PdfParameter::ALL.len(), 9);
        for (p, name, lo, hi, unit) in abs {
            assert_eq!(p.name(), name);
            let PdfBounds::Absolute(b) = p.bounds() else { panic!("{name} must be absolute") };
            assert_eq!((b.hard_min, b.hard_max, b.unit.code(), b.precision), (lo, hi, unit, Some(0)), "{name}");
            // Unknown stays unknown: never mirrored from the opposite bound.
            assert_eq!((b.soft_min, b.soft_max, b.default), (None, None, None), "{name}");
            // Hard bounds inclusive; just outside refused; precision 0; non-finite refused.
            assert_eq!(p.check(lo), Ok(lo));
            assert_eq!(p.check(hi), Ok(hi));
            assert_eq!(p.check(lo - 1.0), Err(PdfExportError::ParameterOutOfBounds(p)));
            assert_eq!(p.check(hi + 1.0), Err(PdfExportError::ParameterOutOfBounds(p)));
            assert_eq!(p.check(lo + 0.5), Err(PdfExportError::ParameterPrecision(p)));
            assert_eq!(p.check(f64::NAN), Err(PdfExportError::ParameterNotFinite(p)));
            assert_eq!(p.check(f64::INFINITY), Err(PdfExportError::ParameterNotFinite(p)));
        }
        for (p, name, partner) in rel {
            assert_eq!(p.name(), name);
            let PdfBounds::Relative(b) = p.bounds() else { panic!("{name} must be relative") };
            assert_eq!(b.partner, partner);
            assert_eq!((b.min_multiple, b.max_multiple), (1.0, 10.0));
            assert_eq!((b.soft_min, b.soft_max, b.default, b.precision), (None, None, None, None), "{name}");
            // The declared unit is kept beside the stored unit (conflict reported, UNVERIFIED).
            assert_eq!((b.declared_unit.code(), b.stored_unit.code()), ("ratio", "dpi"));
            // Live bounds follow the partner value; no absolute maximum exists.
            assert_eq!(b.live(300.0), (300.0, 3000.0));
            assert_eq!(b.live(72.0), (72.0, 720.0));
            assert_eq!(p.check_relative(300.0, Some(300.0)), Ok(300.0));
            assert_eq!(p.check_relative(3000.0, Some(300.0)), Ok(3000.0));
            assert_eq!(p.check_relative(299.0, Some(300.0)), Err(PdfExportError::ThresholdOutOfRelativeBounds(p)));
            assert_eq!(p.check_relative(3001.0, Some(300.0)), Err(PdfExportError::ThresholdOutOfRelativeBounds(p)));
            // The same value is in bounds for one partner and out for another.
            assert_eq!(p.check_relative(3001.0, Some(1200.0)), Ok(3001.0));
            assert_eq!(p.check_relative(450.0, None), Err(PdfExportError::ThresholdPartnerUnset(p)));
            assert_eq!(p.check_relative(f64::NAN, Some(300.0)), Err(PdfExportError::ParameterNotFinite(p)));
            // An absolute check of a relative row is refused (it needs the partner).
            assert!(p.check(450.0).is_err());
        }
        for class in ImageClass::ALL {
            let PdfBounds::Relative(b) = class.threshold_parameter().bounds() else { panic!() };
            assert_eq!(b.partner, class.sampling_parameter());
        }
    });
}

#[test]
fn pdf_numeric_relative_bound_metadata_partner_up() {
    bounded_case("pdf_numeric_relative_bound_metadata_partner_up", || {
        for class in ImageClass::ALL {
            let mut c = CompressionPanel::default();
            c.set_sampling_dpi(class, Some(300.0)).unwrap();
            c.set_threshold(class, Some(450.0)).unwrap();
            assert_eq!(c.threshold_bounds(class), Some((300.0, 3000.0)));
            assert_eq!(c.class(class).threshold_ratio(), Some(1.5));
            // Partner up and the threshold stays inside [1x, 10x]: accepted.
            c.set_sampling_dpi(class, Some(450.0)).unwrap();
            assert_eq!((c.class(class).sampling_dpi(), c.class(class).threshold_dpi()), (Some(450.0), Some(450.0)));
            assert_eq!(c.threshold_bounds(class), Some((450.0, 4500.0)));
            // Partner up past the threshold: refused, the old state kept.
            let before = c.clone();
            assert_eq!(
                c.set_sampling_dpi(class, Some(451.0)),
                Err(PdfExportError::PartnerChangeInvalidatesThreshold(class.threshold_parameter()))
            );
            assert_eq!(c, before);
            // Accept both together: partner and threshold change atomically.
            c.set_sampling_dpi_and_threshold(class, Some(600.0), Some(900.0)).unwrap();
            assert_eq!((c.class(class).sampling_dpi(), c.class(class).threshold_dpi()), (Some(600.0), Some(900.0)));
            // Neither stored when the pair does not hold.
            let before = c.clone();
            assert_eq!(
                c.set_sampling_dpi_and_threshold(class, Some(1200.0), Some(900.0)),
                Err(PdfExportError::ThresholdOutOfRelativeBounds(class.threshold_parameter()))
            );
            assert_eq!(c, before);
            // A partner over its own hard bound is refused before the threshold is looked at.
            assert_eq!(
                c.set_sampling_dpi(class, Some(2401.0)),
                Err(PdfExportError::ParameterOutOfBounds(class.sampling_parameter()))
            );
            assert_eq!(c, before);
        }
    });
}

#[test]
fn pdf_numeric_relative_bound_metadata_partner_down() {
    bounded_case("pdf_numeric_relative_bound_metadata_partner_down", || {
        for class in ImageClass::ALL {
            let mut c = CompressionPanel::default();
            c.set_sampling_dpi_and_threshold(class, Some(300.0), Some(3000.0)).unwrap();
            // Partner down: 10x 299 = 2990 < 3000, refused, state kept.
            let before = c.clone();
            assert_eq!(
                c.set_sampling_dpi(class, Some(299.0)),
                Err(PdfExportError::PartnerChangeInvalidatesThreshold(class.threshold_parameter()))
            );
            assert_eq!(c, before);
            // Clearing the partner while its threshold is set is refused too.
            assert_eq!(
                c.set_sampling_dpi(class, None),
                Err(PdfExportError::PartnerChangeInvalidatesThreshold(class.threshold_parameter()))
            );
            assert_eq!(c, before);
            // Lower the threshold first, then the partner: both accepted.
            c.set_threshold(class, Some(1000.0)).unwrap();
            c.set_sampling_dpi(class, Some(100.0)).unwrap();
            assert_eq!(c.threshold_bounds(class), Some((100.0, 1000.0)));
            assert_eq!(c.class(class).threshold_ratio(), Some(10.0));
            // Clearing the threshold, then the partner.
            c.set_threshold(class, None).unwrap();
            c.set_sampling_dpi(class, None).unwrap();
            assert_eq!(c.class(class), &ImageClassCompression::default());
            // Other classes are untouched.
            for other in ImageClass::ALL.into_iter().filter(|o| *o != class) {
                assert_eq!(c.class(other), &ImageClassCompression::default());
            }
        }
    });
}

#[test]
fn pdf_numeric_relative_bound_metadata_threshold_first() {
    bounded_case("pdf_numeric_relative_bound_metadata_threshold_first", || {
        for class in ImageClass::ALL {
            let mut c = CompressionPanel::default();
            // Threshold first, with no live partner: refused, nothing stored.
            assert_eq!(
                c.set_threshold(class, Some(450.0)),
                Err(PdfExportError::ThresholdPartnerUnset(class.threshold_parameter()))
            );
            assert_eq!(c, CompressionPanel::default());
            assert_eq!(c.threshold_bounds(class), None);
            // Partner then threshold; the bound is checked against the live partner.
            c.set_sampling_dpi(class, Some(150.0)).unwrap();
            assert_eq!(
                c.set_threshold(class, Some(149.0)),
                Err(PdfExportError::ThresholdOutOfRelativeBounds(class.threshold_parameter()))
            );
            assert_eq!(
                c.set_threshold(class, Some(1501.0)),
                Err(PdfExportError::ThresholdOutOfRelativeBounds(class.threshold_parameter()))
            );
            c.set_threshold(class, Some(1500.0)).unwrap();
            c.set_threshold(class, Some(150.0)).unwrap();
            assert_eq!(
                c.set_threshold(class, Some(f64::INFINITY)),
                Err(PdfExportError::ParameterNotFinite(class.threshold_parameter()))
            );
            assert_eq!(c.class(class).threshold_dpi(), Some(150.0));
            let mut prefs = PdfExportPreferences::default();
            prefs.compression = c;
            assert_eq!(prefs.validate(), Ok(()));
        }
    });
}

// ---- MT-30835 General ----

#[test]
fn pdf_general_document_structure_options() {
    bounded_case("pdf_general_document_structure_options", || {
        assert_eq!(PdfPanel::ALL.map(PdfPanel::code), ["general", "compression", "marks_and_bleeds", "output", "advanced", "security"]);
        assert_eq!(
            PdfPanel::General.fields(),
            [
                "acrobat_compatibility", "standards_compliance", "page_range", "export_reader_spreads",
                "export_as_single_pages", "single_pages_pdf_suffix", "export_hidden_spread", "export_layers",
                "include_bookmarks", "include_hyperlinks", "include_structure", "export_nonprinting_objects",
                "export_guides_and_grids", "interactive_elements_option", "pdf_magnification", "pdf_page_layout",
                "open_in_full_screen", "pdf_display_title", "default_document_language", "generate_thumbnails",
                "optimize_pdf", "view_pdf", "preserve_editing_capabilities",
            ]
        );
        assert_eq!(
            AcrobatCompatibility::ALL.iter().map(|m| m.code()).collect::<Vec<_>>(),
            ["acrobat_4", "acrobat_5", "acrobat_6", "acrobat_7", "acrobat_8"]
        );
        assert_eq!(
            ExportLayers::ALL.iter().map(|m| m.code()).collect::<Vec<_>>(),
            ["layers.all", "layers.visible", "layers.visible_printable"]
        );
        assert_eq!(
            PdfDisplayTitle::ALL.iter().map(|m| m.code()).collect::<Vec<_>>(),
            ["display.file_name", "display.document_title"]
        );
        for m in AcrobatCompatibility::ALL {
            assert_eq!(AcrobatCompatibility::from_code(m.code()), Ok(*m));
        }
        for m in ExportLayers::ALL {
            assert_eq!(ExportLayers::from_code(m.code()), Ok(*m));
        }
        for m in PdfDisplayTitle::ALL {
            assert_eq!(PdfDisplayTitle::from_code(m.code()), Ok(*m));
        }
        // Exact codes only: no case folding, trimming or aliasing.
        for bad in ["Acrobat_4", " acrobat_4", "acrobat_9", "1.3", ""] {
            assert_eq!(AcrobatCompatibility::from_code(bad), Err(PdfExportError::UnknownMember("AcrobatCompatibility")));
        }
        assert!(ExportLayers::from_code("layers.All").is_err());
        // Undeclared domains: bounded tokens, no invented members.
        for f in [PdfTokenField::InteractiveElementsOption, PdfTokenField::PdfMagnification, PdfTokenField::PdfPageLayout] {
            assert_eq!(f.domain_completeness(), DomainCompleteness::Undeclared);
            assert!(f.known_members().is_empty());
        }
        assert_eq!(PdfTokenField::PdfMagnification.declared_member_count(), Some(10));
        assert_eq!(PdfTokenField::PdfPageLayout.declared_member_count(), None);
        // standards_compliance: partial domain with the four STU-LAY-159 compliance tokens.
        let sc = PdfTokenField::StandardsCompliance;
        assert_eq!(sc.domain_completeness().code(), "partial");
        assert_eq!(sc.known_members(), ["none", "PDF/X-1a:2001", "PDF/X-3:2002", "PDF/X-4:2007"]);
        assert!(token("PDF/X-4:2007").is_known_member_of(sc));
        assert!(!token("pdf/x-4:2007").is_known_member_of(sc));
        assert!(!token("PDF/X-4").is_known_member_of(sc));
        assert!(!token("PDF/X-4:2007").is_known_member_of(PdfTokenField::PdfPageLayout));
        // page_range: booklet grammar, resolved against a document on demand.
        let range = PdfPageRange::new("3-5, 1").unwrap();
        assert_eq!(range.resolve(5), Ok(vec![3, 4, 5, 1]));
        assert_eq!(range.resolve(4).map_err(|e| e.code()), Err("page_range_out_of_document"));
        assert_eq!(PdfPageRange::new("").map_err(|e| e.code()), Err("page_range_empty"));
        assert_eq!(PdfPageRange::new("5-3").map_err(|e| e.code()), Err("page_range_malformed"));
        assert_eq!(PdfPageRange::new("0").map_err(|e| e.code()), Err("page_range_malformed"));
        // A whole General panel validates; a non-finite or oversized captured value is refused.
        let mut prefs = PdfExportPreferences::default();
        let g = &mut prefs.general;
        g.acrobat_compatibility = Some(AcrobatCompatibility::Acrobat7);
        g.standards_compliance = Some(token("PDF/X-1a:2001"));
        g.page_range = Some(range);
        g.export_as_single_pages = Some(PrintValue::Bool(true));
        g.single_pages_pdf_suffix = Some(PrintValue::Text("_page".into()));
        g.export_layers = Some(ExportLayers::VisiblePrintable);
        g.pdf_magnification = Some(token("fit_page"));
        g.pdf_display_title = Some(PdfDisplayTitle::DocumentTitle);
        g.default_document_language = Some(PrintValue::Text("en-US".into()));
        assert_eq!(prefs.validate(), Ok(()));
        prefs.general.view_pdf = Some(PrintValue::Number(f64::NAN));
        assert_eq!(prefs.validate(), Err(PdfExportError::FieldNotFinite("view_pdf")));
        prefs.general.view_pdf = None;
        prefs.general.single_pages_pdf_suffix = Some(PrintValue::Text("x".repeat(MAX_PRINT_TEXT_BYTES + 1)));
        assert_eq!(prefs.validate(), Err(PdfExportError::TextTooLong("single_pages_pdf_suffix")));
    });
}

// ---- MT-30836 Compression ----

#[test]
fn pdf_compression_classes_quality_tiles() {
    bounded_case("pdf_compression_classes_quality_tiles", || {
        assert_eq!(
            PdfPanel::Compression.fields(),
            [
                "color_sampling_method", "color_bitmap_sampling_dpi", "color_compression_method",
                "color_compression_quality", "threshold_to_compress_color",
                "grayscale_sampling_method", "grayscale_bitmap_sampling_dpi", "grayscale_compression_method",
                "grayscale_compression_quality", "threshold_to_compress_gray",
                "monochrome_sampling_method", "monochrome_bitmap_sampling_dpi", "monochrome_compression_method",
                "monochrome_compression_quality", "threshold_to_compress_monochrome",
                "compress_text_and_line_art", "crop_images_to_frames", "compression_type", "color_tile_size",
                "gray_tile_size",
            ]
        );
        assert_eq!(
            CompressionQuality::ALL.iter().map(|m| m.code()).collect::<Vec<_>>(),
            [
                "compression_quality.minimum", "compression_quality.low", "compression_quality.medium",
                "compression_quality.high", "compression_quality.maximum", "compression_quality.four_bit",
                "compression_quality.eight_bit",
            ]
        );
        assert_eq!(
            CompressionType::ALL.iter().map(|m| m.code()).collect::<Vec<_>>(),
            ["compress.none", "compress.structure", "compress.objects"]
        );
        for m in CompressionQuality::ALL {
            assert_eq!(CompressionQuality::from_code(m.code()), Ok(*m));
        }
        for m in CompressionType::ALL {
            assert_eq!(CompressionType::from_code(m.code()), Ok(*m));
        }
        assert!(CompressionQuality::from_code("compression_quality.MAXIMUM").is_err());
        assert!(CompressionType::from_code("none").is_err());
        // Sampling and compression methods name no members.
        for f in [
            PdfTokenField::ColorSamplingMethod,
            PdfTokenField::GrayscaleSamplingMethod,
            PdfTokenField::MonochromeSamplingMethod,
            PdfTokenField::ColorCompressionMethod,
            PdfTokenField::GrayscaleCompressionMethod,
            PdfTokenField::MonochromeCompressionMethod,
        ] {
            assert_eq!(f.domain_completeness(), DomainCompleteness::Undeclared, "{}", f.name());
            assert_eq!(f.declared_member_count(), None);
        }
        let mut prefs = PdfExportPreferences::default();
        let c = &mut prefs.compression;
        c.color.sampling_method = Some(token("bicubic_downsample"));
        c.color.compression_method = Some(token("jpeg"));
        c.color.quality = Some(CompressionQuality::Maximum);
        c.monochrome.quality = Some(CompressionQuality::FourBit);
        c.set_sampling_dpi_and_threshold(ImageClass::Color, Some(300.0), Some(450.0)).unwrap();
        c.compression_type = Some(CompressionType::Objects);
        c.compress_text_and_line_art = Some(PrintValue::Bool(true));
        c.color_tile_size = Some(256.0);
        c.gray_tile_size = Some(2048.0);
        assert_eq!(prefs.validate(), Ok(()));
        assert_eq!(prefs.compression.class(ImageClass::Color).quality, Some(CompressionQuality::Maximum));
        assert_eq!(prefs.compression.class(ImageClass::Grayscale).quality, None);
        // Tile sizes against 128..=2048 px, precision 0.
        prefs.compression.color_tile_size = Some(127.0);
        assert_eq!(prefs.validate(), Err(PdfExportError::ParameterOutOfBounds(PdfParameter::ColorTileSize)));
        prefs.compression.color_tile_size = Some(256.5);
        assert_eq!(prefs.validate(), Err(PdfExportError::ParameterPrecision(PdfParameter::ColorTileSize)));
        prefs.compression.color_tile_size = Some(256.0);
        prefs.compression.gray_tile_size = Some(2049.0);
        assert_eq!(prefs.validate(), Err(PdfExportError::ParameterOutOfBounds(PdfParameter::GrayTileSize)));
        prefs.compression.gray_tile_size = None;
        // A sampling dpi with a fraction is refused (precision 0) and nothing changes.
        let before = prefs.clone();
        assert_eq!(
            prefs.compression.set_sampling_dpi(ImageClass::Grayscale, Some(150.5)),
            Err(PdfExportError::ParameterPrecision(PdfParameter::GrayscaleBitmapSamplingDpi))
        );
        assert_eq!(prefs, before);
    });
}

// ---- MT-30837 Marks and bleeds ----

#[test]
fn pdf_marks_bleed_nine_weights() {
    bounded_case("pdf_marks_bleed_nine_weights", || {
        assert_eq!(
            PdfPanel::MarksAndBleeds.fields(),
            [
                "crop_marks", "bleed_marks", "registration_marks", "color_bars", "page_information_marks",
                "page_marks_offset", "printer_mark_weight", "pdf_mark_type", "bleed_top", "bleed_bottom",
                "bleed_inside", "bleed_outside", "use_document_bleed_with_pdf", "include_slug_with_pdf",
            ]
        );
        // Code, unit, declared fraction, exact points (hand-computed: 1 mm = 360/127 pt).
        type WeightRow = (&'static str, MarkWeightUnit, (u32, u32), (u32, u32));
        let rows: [WeightRow; 9] = [
            ("0.125pt", MarkWeightUnit::Pt, (1, 8), (1, 8)),
            ("0.25pt", MarkWeightUnit::Pt, (1, 4), (1, 4)),
            ("0.50pt", MarkWeightUnit::Pt, (1, 2), (1, 2)),
            ("0.05mm", MarkWeightUnit::Mm, (1, 20), (18, 127)),
            ("0.07mm", MarkWeightUnit::Mm, (7, 100), (126, 635)),
            ("0.10mm", MarkWeightUnit::Mm, (1, 10), (36, 127)),
            ("0.15mm", MarkWeightUnit::Mm, (3, 20), (54, 127)),
            ("0.20mm", MarkWeightUnit::Mm, (1, 5), (72, 127)),
            ("0.30mm", MarkWeightUnit::Mm, (3, 10), (108, 127)),
        ];
        assert_eq!(PrinterMarkWeight::ALL.len(), 9);
        for (member, (code, unit, declared, points)) in PrinterMarkWeight::ALL.iter().zip(rows) {
            assert_eq!(member.code(), code);
            assert_eq!(PrinterMarkWeight::from_code(code), Ok(*member));
            assert_eq!(member.unit(), unit, "{code}");
            assert_eq!(member.declared(), declared, "{code}");
            assert_eq!(member.points_exact(), points, "{code}");
            // The point fraction equals the declared magnitude in its unit, cross-multiplied exactly.
            let (a, b) = declared;
            let (n, d) = points;
            let (num, den) = match unit {
                MarkWeightUnit::Pt => (u64::from(a), u64::from(b)),
                MarkWeightUnit::Mm => (u64::from(a) * 360, u64::from(b) * 127),
            };
            assert_eq!(u64::from(n) * den, num * u64::from(d), "{code}");
        }
        // Never merged: nine distinct point values (0.25pt vs 0.10mm = 0.2834 pt stay separate).
        let mut pts: Vec<f64> = PrinterMarkWeight::ALL.iter().map(|m| m.points()).collect();
        pts.sort_by(f64::total_cmp);
        pts.dedup();
        assert_eq!(pts.len(), 9);
        assert!((PrinterMarkWeight::Mm010.points() - 0.283_464_566_929_133_9).abs() < 1e-15);
        for bad in ["0.5pt", "0.125 pt", "0.10MM", "0.1mm"] {
            assert!(PrinterMarkWeight::from_code(bad).is_err(), "{bad}");
        }
        assert_eq!(PdfTokenField::PdfMarkType.domain_completeness(), DomainCompleteness::Undeclared);
        // Four bleeds (finite >= 0 pt) and use_document_bleed_with_pdf.
        let mut prefs = PdfExportPreferences::default();
        let m = &mut prefs.marks_and_bleeds;
        m.crop_marks = Some(PrintValue::Bool(true));
        m.printer_mark_weight = Some(PrinterMarkWeight::Mm007);
        m.bleed_top = Some(9.0);
        m.bleed_bottom = Some(0.0);
        m.bleed_inside = Some(4.5);
        m.bleed_outside = Some(9.0);
        m.use_document_bleed_with_pdf = Some(PrintValue::Bool(false));
        assert_eq!(prefs.validate(), Ok(()));
        let setters: [(&str, SetBleed); 4] = [
            ("bleed_top", |m, v| m.bleed_top = Some(v)),
            ("bleed_bottom", |m, v| m.bleed_bottom = Some(v)),
            ("bleed_inside", |m, v| m.bleed_inside = Some(v)),
            ("bleed_outside", |m, v| m.bleed_outside = Some(v)),
        ];
        for (name, set) in setters {
            for bad in [-0.5, f64::NAN, f64::NEG_INFINITY] {
                let mut p = prefs.clone();
                set(&mut p.marks_and_bleeds, bad);
                assert_eq!(p.validate(), Err(PdfExportError::BleedInvalid(name)), "{name} {bad}");
            }
        }
    });
}

// ---- MT-30838 Output ----

#[test]
fn pdf_output_effective_colour_readback() {
    bounded_case("pdf_output_effective_colour_readback", || {
        assert_eq!(
            PdfPanel::Output.fields(),
            [
                "pdf_color_space", "pdf_destination_profile", "include_icc_profiles", "pdf_x_profile",
                "output_condition", "output_condition_name", "oc_registry", "simulate_overprint",
            ]
        );
        assert_eq!(
            PdfColorSpace::ALL.iter().map(|m| m.code()).collect::<Vec<_>>(),
            [
                "pdf_color.rgb", "pdf_color.cmyk", "pdf_color.unchanged", "pdf_color.repurpose_rgb",
                "pdf_color.repurpose_cmyk", "pdf_color.gray",
            ]
        );
        for m in PdfColorSpace::ALL {
            assert_eq!(PdfColorSpace::from_code(m.code()), Ok(*m));
        }
        assert!(PdfColorSpace::from_code("pdf_color.RGB").is_err());
        assert_eq!(
            PdfEffectiveField::ALL.map(PdfEffectiveField::name),
            ["effective_destination_profile", "effective_output_condition", "effective_oc_registry", "effective_pdf_x_profile"]
        );
        // A request is never echoed into an effective field.
        let mut prefs = PdfExportPreferences::default();
        let o = &mut prefs.output;
        o.pdf_color_space = Some(PdfColorSpace::RepurposeCmyk);
        o.pdf_destination_profile = Some(PrintValue::Text("Coated FOGRA39".into()));
        o.pdf_x_profile = Some(PrintValue::Text("Coated FOGRA39".into()));
        o.output_condition = Some(PrintValue::Text("offset".into()));
        o.output_condition_name = Some(PrintValue::Text("FOGRA39".into()));
        o.oc_registry = Some(PrintValue::Text("color_org_registry".into()));
        assert_eq!(prefs.validate(), Ok(()));
        assert!(prefs.effective().is_empty());
        for f in PdfEffectiveField::ALL {
            assert_eq!(prefs.effective().get(f), None, "{}", f.name());
        }
        // The exporter records what it resolved; requested values are untouched.
        let requested = prefs.output.clone();
        prefs.record_effective(PdfEffectiveField::DestinationProfile, "sRGB IEC61966-2.1").unwrap();
        prefs.record_effective(PdfEffectiveField::PdfXProfile, "").unwrap();
        assert_eq!(prefs.output, requested);
        assert_eq!(prefs.effective().get(PdfEffectiveField::DestinationProfile), Some("sRGB IEC61966-2.1"));
        assert_eq!(prefs.effective().get(PdfEffectiveField::PdfXProfile), Some(""));
        assert_eq!(prefs.effective().get(PdfEffectiveField::OutputCondition), None);
        assert_eq!(prefs.effective().get(PdfEffectiveField::OcRegistry), None);
        assert_eq!(
            prefs.record_effective(PdfEffectiveField::OcRegistry, &"r".repeat(MAX_PRINT_TEXT_BYTES + 1)),
            Err(PdfExportError::TextTooLong("effective_oc_registry"))
        );
        assert_eq!(prefs.effective().get(PdfEffectiveField::OcRegistry), None);
        // The requested state drops the read-back and keeps every request.
        let state = prefs.requested_state();
        assert!(state.effective().is_empty());
        assert_eq!(state.output, requested);
    });
}

// ---- MT-30839 Advanced ----

#[test]
fn pdf_advanced_subset_omit_flatten() {
    bounded_case("pdf_advanced_subset_omit_flatten", || {
        assert_eq!(
            PdfPanel::Advanced.fields(),
            ["subset_fonts_below", "omit_bitmaps", "omit_eps", "omit_pdf", "applied_flattener_preset", "ignore_spread_overrides"]
        );
        let mut prefs = PdfExportPreferences::default();
        let a = &mut prefs.advanced;
        a.subset_fonts_below = Some(100.0);
        a.omit_bitmaps = Some(PrintValue::Bool(false));
        a.omit_eps = Some(PrintValue::Bool(true));
        a.omit_pdf = Some(PrintValue::Bool(false));
        a.applied_flattener_preset = Some(PdfName::new("[High Resolution]").unwrap());
        a.ignore_spread_overrides = Some(PrintValue::Bool(true));
        assert_eq!(prefs.validate(), Ok(()));
        prefs.advanced.subset_fonts_below = Some(0.0);
        assert_eq!(prefs.validate(), Ok(()));
        prefs.advanced.subset_fonts_below = Some(100.5);
        assert_eq!(prefs.validate(), Err(PdfExportError::ParameterOutOfBounds(PdfParameter::SubsetFontsBelow)));
        prefs.advanced.subset_fonts_below = Some(35.5);
        assert_eq!(prefs.validate(), Err(PdfExportError::ParameterPrecision(PdfParameter::SubsetFontsBelow)));
        prefs.advanced.subset_fonts_below = Some(-1.0);
        assert_eq!(prefs.validate(), Err(PdfExportError::ParameterOutOfBounds(PdfParameter::SubsetFontsBelow)));
        // applied_flattener_preset is a bounded name.
        assert_eq!(PdfName::new("   "), Err(PdfExportError::NameEmpty));
        assert_eq!(PdfName::new(&"n".repeat(MAX_PDF_NAME_BYTES + 1)), Err(PdfExportError::NameTooLong));
        assert_eq!(PdfName::new("a\u{7}b"), Err(PdfExportError::NameInvalid));
        assert_eq!(PdfName::new(&"n".repeat(MAX_PDF_NAME_BYTES)).map(|n| n.as_str().len()), Ok(MAX_PDF_NAME_BYTES));
    });
}

// ---- MT-30840 Security; MT-30833 AC-003 ----

#[test]
fn pdf_security_password_permission_bits() {
    bounded_case("pdf_security_password_permission_bits", || {
        assert_eq!(
            PdfPanel::Security.fields(),
            [
                "use_security", "open_document_password", "change_security_password", "disallow_printing",
                "disallow_hi_res_printing", "disallow_changing", "disallow_copying",
                "disallow_extraction_for_accessibility", "disallow_document_assembly", "disallow_notes",
                "disallow_form_fill_in", "disallow_plaintext_metadata",
            ]
        );
        assert_eq!(&PdfPanel::Security.fields()[3..], PdfPermission::ALL.map(PdfPermission::name));
        let mut s = SecurityPanel::default();
        // Security unset or off: reading a password is the typed error, never an empty string.
        for use_security in [None, Some(false)] {
            s.set_use_security(use_security);
            for f in [PdfPasswordField::OpenDocument, PdfPasswordField::ChangeSecurity] {
                assert_eq!(s.password(f), Err(PdfExportError::PasswordSecurityOff), "{}", f.name());
                assert_eq!(
                    s.set_password(f, Some(PdfPassword::new("open-sesame").unwrap())),
                    Err(PdfExportError::PasswordSecurityOff)
                );
            }
        }
        assert_eq!(PdfExportError::PasswordSecurityOff.code(), "pdf_password_security_off");
        // Security on: no password yet is None, then the set value.
        s.set_use_security(Some(true));
        assert_eq!(s.password(PdfPasswordField::OpenDocument), Ok(None));
        s.set_password(PdfPasswordField::OpenDocument, Some(PdfPassword::new("open-sesame").unwrap())).unwrap();
        s.set_password(PdfPasswordField::ChangeSecurity, Some(PdfPassword::new("owner-key").unwrap())).unwrap();
        assert_eq!(s.password(PdfPasswordField::OpenDocument).unwrap().map(PdfPassword::expose), Some("open-sesame"));
        assert_eq!(s.password(PdfPasswordField::ChangeSecurity).unwrap().map(PdfPassword::expose), Some("owner-key"));
        // Turning security off clears both (Studio reading); turning it back on shows no old value.
        s.set_use_security(Some(false));
        assert_eq!(s.password(PdfPasswordField::OpenDocument), Err(PdfExportError::PasswordSecurityOff));
        s.set_use_security(Some(true));
        assert_eq!(s.password(PdfPasswordField::OpenDocument), Ok(None));
        assert_eq!(s.password(PdfPasswordField::ChangeSecurity), Ok(None));
        // Password admission.
        assert_eq!(PdfPassword::new(""), Err(PdfExportError::PasswordEmpty));
        assert_eq!(PdfPassword::new(&"p".repeat(MAX_PDF_PASSWORD_BYTES + 1)), Err(PdfExportError::PasswordTooLong));
        // The nine disallow_* flags are independent.
        for (i, p) in PdfPermission::ALL.into_iter().enumerate() {
            let mut s = SecurityPanel::default();
            s.set_permission(p, Some(true));
            for (j, q) in PdfPermission::ALL.into_iter().enumerate() {
                assert_eq!(s.permission(q), if i == j { Some(true) } else { None }, "{} vs {}", p.name(), q.name());
            }
            s.set_permission(p, Some(false));
            assert_eq!(s.permission(p), Some(false));
        }
        let mut s = SecurityPanel::default();
        for p in PdfPermission::ALL {
            s.set_permission(p, Some(true));
        }
        s.set_permission(PdfPermission::Copying, Some(false));
        assert_eq!(s.disallow_copying, Some(false));
        assert_eq!(PdfPermission::ALL.iter().filter(|p| s.permission(**p) == Some(true)).count(), 8);
    });
}

#[test]
fn pdf_security_password_permission_bits_redacted_everywhere() {
    bounded_case("pdf_security_password_permission_bits_redacted_everywhere", || {
        const SECRET: &str = "Hunter2-Zq9!secret";
        let mut prefs = PdfExportPreferences::default();
        prefs.security.set_use_security(Some(true));
        prefs.security.set_password(PdfPasswordField::OpenDocument, Some(PdfPassword::new(SECRET).unwrap())).unwrap();
        prefs.security.set_password(PdfPasswordField::ChangeSecurity, Some(PdfPassword::new(SECRET).unwrap())).unwrap();
        // Debug of the password, the panel and the whole record carries no value.
        let password_debug = format!("{:?}", PdfPassword::new(SECRET).unwrap());
        assert_eq!(password_debug, "PdfPassword(<redacted>)");
        for text in [format!("{prefs:?}"), format!("{:#?}", prefs.security), format!("{:?}", PdfExportPreset::new("p", &prefs).unwrap())] {
            assert!(!text.contains(SECRET), "password leaked through Debug");
            assert!(!text.contains("Hunter2"), "password prefix leaked through Debug");
        }
        // Errors are codes; observe frames and receipts carry no value.
        let token = CancellationToken::default();
        let mut sink = Collect(Vec::new());
        let ok: Result<PdfExportPreferences, LayoutError> = Ok(prefs.clone());
        let receipt = report(&ok, 1, 3, &token, &mut observe(), &mut sink).unwrap();
        assert_eq!(receipt.outcome, Outcome::Success);
        let mut off = SecurityPanel::default();
        let refused = off
            .set_password(PdfPasswordField::OpenDocument, Some(PdfPassword::new(SECRET).unwrap()))
            .map_err(PdfExportError::layout_error);
        assert_eq!(refused, Err(LayoutError::InvalidInput));
        let receipt = report(&refused, 1, 3, &token, &mut observe(), &mut sink).unwrap();
        assert_eq!(receipt.outcome, Outcome::Failure(FailureCode::Validation));
        assert!(!format!("{receipt:?}").contains(SECRET));
        for frame in &sink.0 {
            assert!(!contains(frame, SECRET.as_bytes()), "password in an observe frame");
        }
        assert!(!format!("{:?}", PdfExportError::PasswordSecurityOff).contains(SECRET));
    });
}

// ---- STU-LAY-159 preset reference (lead placement) and the record ----

#[test]
fn pdf_export_preset_reference_and_complete_copy() {
    bounded_case("pdf_export_preset_reference_and_complete_copy", || {
        // A bounded id string; resolves nothing.
        let id = PdfPresetId::new("hsk.studio.pdf_preset.pdf_x_4").unwrap();
        assert_eq!(id.as_str(), "hsk.studio.pdf_preset.pdf_x_4");
        for bad in ["", "Hsk.Studio", "has space", "hsk/studio", "pdf\u{0}x"] {
            assert_eq!(PdfPresetId::new(bad), Err(PdfExportError::PresetIdInvalid), "{bad:?}");
        }
        assert_eq!(PdfPresetId::new(&"a".repeat(MAX_PDF_PRESET_ID_BYTES + 1)), Err(PdfExportError::PresetIdTooLong));
        // A complete state saved under a name and applied as a full copy (read-back dropped).
        let mut prefs = PdfExportPreferences::default();
        prefs.pdf_export_preset_id = Some(id.clone());
        prefs.general.acrobat_compatibility = Some(AcrobatCompatibility::Acrobat5);
        prefs.compression.set_sampling_dpi_and_threshold(ImageClass::Monochrome, Some(1200.0), Some(1800.0)).unwrap();
        prefs.marks_and_bleeds.printer_mark_weight = Some(PrinterMarkWeight::Pt025);
        prefs.output.pdf_color_space = Some(PdfColorSpace::Cmyk);
        prefs.advanced.subset_fonts_below = Some(100.0);
        prefs.security.set_use_security(Some(true));
        prefs.security.disallow_notes = Some(true);
        prefs.record_effective(PdfEffectiveField::OutputCondition, "resolved").unwrap();
        let preset = PdfExportPreset::new("Press handoff", &prefs).unwrap();
        assert_eq!(preset.name.as_str(), "Press handoff");
        let applied = preset.apply();
        assert_eq!(applied, prefs.requested_state());
        assert!(applied.effective().is_empty());
        assert_eq!(applied.pdf_export_preset_id, Some(id));
        assert_eq!(applied.compression.class(ImageClass::Monochrome).threshold_ratio(), Some(1.5));
        // The preset is unchanged by edits of the applied copy.
        let mut edited = applied;
        edited.general.acrobat_compatibility = Some(AcrobatCompatibility::Acrobat8);
        assert_ne!(edited, preset.apply());
        assert_eq!(preset.preferences().general.acrobat_compatibility, Some(AcrobatCompatibility::Acrobat5));
        // An invalid state or name is refused.
        let mut bad = prefs.clone();
        bad.advanced.subset_fonts_below = Some(101.0);
        assert_eq!(
            PdfExportPreset::new("x", &bad),
            Err(PdfExportError::ParameterOutOfBounds(PdfParameter::SubsetFontsBelow))
        );
        assert_eq!(PdfExportPreset::new("", &prefs), Err(PdfExportError::NameEmpty));
        // Reported through layout report with codes only.
        let token = CancellationToken::default();
        let refused = PdfExportPreset::new("x", &bad).map_err(PdfExportError::layout_error);
        let receipt = report(&refused, 1, 3, &token, &mut observe(), &mut Collect(Vec::new())).unwrap();
        assert_eq!(receipt.outcome, Outcome::Failure(FailureCode::Validation));
        let codes = [
            PdfExportError::ParameterNotFinite(PdfParameter::GrayTileSize).code(),
            PdfExportError::ThresholdPartnerUnset(PdfParameter::ThresholdToCompressGray).code(),
            PdfExportError::ThresholdOutOfRelativeBounds(PdfParameter::ThresholdToCompressGray).code(),
            PdfExportError::PartnerChangeInvalidatesThreshold(PdfParameter::ThresholdToCompressGray).code(),
            PdfExportError::UnknownMember("PdfColorSpace").code(),
            PdfExportError::TokenTooLong.code(),
            PdfExportError::PresetIdInvalid.code(),
            PdfExportError::PasswordTooLong.code(),
            PdfExportError::BleedInvalid("bleed_top").code(),
            PdfExportError::TextTooLong("view_pdf").code(),
        ];
        for c in codes {
            assert!(!c.is_empty() && c.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'), "{c}");
        }
    });
}

// ---- Hostile input (V4-01/02) ----

#[test]
fn pdf_export_text_dims_u32_max_is_refused_before_allocation() {
    bounded_case("pdf_export_text_dims_u32_max_is_refused_before_allocation", || {
        let long = "x".repeat(MAX_PDF_TOKEN_BYTES + 1);
        assert_eq!(PdfToken::new(&long), Err(PdfExportError::TokenTooLong));
        assert_eq!(PdfToken::new(&"x".repeat(MAX_PDF_TOKEN_BYTES)).map(|t| t.as_str().len()), Ok(MAX_PDF_TOKEN_BYTES));
        assert_eq!(PdfToken::new(""), Err(PdfExportError::TokenEmpty));
        assert_eq!(PdfToken::new(" \t "), Err(PdfExportError::TokenEmpty));
        assert_eq!(PdfToken::new("fit\npage"), Err(PdfExportError::TokenInvalid));
        // Tokens are kept exactly (no trimming or folding).
        assert_eq!(token(" Fit ").as_str(), " Fit ");
        assert_eq!(PdfExportError::TokenTooLong.layout_error(), LayoutError::BudgetExceeded);
        assert_eq!(PdfExportError::PasswordTooLong.layout_error(), LayoutError::BudgetExceeded);
        let range = "1,".repeat(MAX_PAGE_RANGE_BYTES);
        assert_eq!(PdfPageRange::new(&range).map_err(|e| e.code()), Err("page_range_too_long"));
        assert_eq!(PdfPageRange::new(&range).map_err(|e| e.layout_error()), Err(LayoutError::BudgetExceeded));
        // A page ordinal past u32 is malformed, never wrapped.
        assert_eq!(PdfPageRange::new("4294967296").map_err(|e| e.code()), Err("page_range_malformed"));
        assert_eq!(PdfPageRange::new(&u32::MAX.to_string()).map_err(|e| e.code()), Err("page_range_out_of_document"));
    });
}

#[test]
fn pdf_export_page_range_aggregate_over_budget_is_refused_before_allocation() {
    bounded_case("pdf_export_page_range_aggregate_over_budget_is_refused_before_allocation", || {
        // Each run is in bounds; the running count passes MAX_IMPOSITION_PAGES on the third.
        let range = "1-50000,1-50000,1-1";
        assert_eq!(PdfPageRange::new(range).map_err(|e| e.code()), Err("page_count_over_bound"));
        assert_eq!(PdfPageRange::new("1-50000,1-50000").map(|r| r.as_str().len()), Ok(15));
        assert_eq!(MAX_IMPOSITION_PAGES, 100_000);
    });
}
