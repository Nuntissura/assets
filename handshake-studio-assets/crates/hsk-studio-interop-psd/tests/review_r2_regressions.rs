//! One regression test per CONFIRMED finding of the independent round-2 review
//! (validation/R2-review-a.json, commit 87c1cd9, lane interop-psd).

mod common;

use common::*;
use hsk_studio_accord::CancellationToken;
use hsk_studio_interop_psd::*;
use hsk_studio_pigment::blend_math::StudioBlendMode as Pigment;

fn rect_bytes(r: [i32; 4]) -> Vec<u8> {
    r.iter().flat_map(|v| v.to_be_bytes()).collect()
}

fn minimal(layer_section: Vec<u8>, image: Option<(u16, Vec<u8>)>) -> Doc {
    Doc {
        version: 1,
        channels: 3,
        height: 1,
        width: 1,
        depth: 8,
        mode: 3,
        color_mode_data: Vec::new(),
        resources: Vec::new(),
        layer_section,
        image,
    }
}

/// Finding 1 (CX-CAP-001): no second blend enumeration; PSD keys map onto Pigment's.
#[test]
fn psd_blend_keys_map_to_pigment_and_no_second_enum_exists() {
    bounded_case("psd_blend_keys_map_to_pigment_and_no_second_enum_exists", || {
        assert_eq!(PSD_BLEND_KEYS.len(), 28);
        let mut keys = std::collections::BTreeSet::new();
        let mut members = std::collections::BTreeSet::new();
        for (key, mode) in PSD_BLEND_KEYS {
            assert!(keys.insert(key) && members.insert(mode.discriminant()), "one key per member");
            assert_eq!(studio_blend_for_key(key), Some(mode));
            assert_eq!(psd_key_for(mode), Some(key));
            assert_eq!(Pigment::try_from(mode.discriminant()), Ok(mode), "same enumeration as Pigment");
        }
        assert_eq!(studio_blend_for_key(*b"xxxx"), None, "an unknown key has no member (typed loss, never Normal)");
        assert_eq!(psd_key_for(Pigment::Behind), None);
        assert_eq!(psd_key_for(Pigment::Average), None);

        // the layer accessor returns Pigment's type
        let mut l = TLayer::new("m", [0, 0, 1, 1]);
        l.key = *b"mul ";
        l.channels = vec![(0, 0, vec![1]), (1, 0, vec![2]), (2, 0, vec![3])];
        let info = layer_info(&[l], false, false);
        let doc = read(&minimal(layer_section(&info, &[], false), Some((0, vec![1, 2, 3]))).bytes());
        let mode: Option<Pigment> = doc.layers()[0].blend_mode();
        assert_eq!(mode, Some(Pigment::Multiply));
        assert_eq!(doc.layers()[0].blend_key, *b"mul ", "raw key bytes stay preserved");

        // the validator control: no enum definition of a blend mode anywhere in this crate's source
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        for entry in std::fs::read_dir(src).unwrap() {
            let path = entry.unwrap().path();
            let text = std::fs::read_to_string(&path).unwrap();
            assert!(!text.contains("enum BlendMode") && !text.contains("enum StudioBlendMode"), "{path:?} defines a blend enum");
        }
    });
}

/// Finding 2: a single byte after the layer section is kept and re-emitted.
#[test]
fn one_trailing_byte_after_the_layer_section_round_trips() {
    bounded_case("one_trailing_byte_after_the_layer_section_round_trips", || {
        let limits = Limits::default();
        let mut bytes = minimal(Vec::new(), None).bytes();
        let plain = bytes.clone();
        bytes.push(0x5A);
        let doc = read(&bytes);
        assert!(doc.image_data.is_none());
        assert_eq!(doc.tail, [0x5A]);
        assert_eq!(write_psd(&doc, &limits).unwrap(), bytes, "byte-identical re-emission");
        assert!(doc.loss_report(&limits).has_code("file_tail_preserved"));
        // with no stray byte nothing changes, and with an image-data section the tail stays empty
        let doc = read(&plain);
        assert!(doc.tail.is_empty() && write_psd(&doc, &limits).unwrap() == plain);
        let with_image = minimal(Vec::new(), Some((0, vec![1, 2, 3]))).bytes();
        let doc = read(&with_image);
        assert!(doc.tail.is_empty() && doc.image_data.is_some());
        assert_eq!(write_psd(&doc, &limits).unwrap(), with_image);
    });
}

/// Finding 3: the real user mask rectangle is found when mask flags bit 4 (parameters) is set,
/// in the specification order (parameters first) and in the order Photoshop-written files use
/// (real-mask fields first).
#[test]
fn real_mask_rect_is_found_when_mask_parameters_are_present() {
    bounded_case("real_mask_rect_is_found_when_mask_parameters_are_present", || {
        let limits = Limits::default();
        let cancel = CancellationToken::default();
        let user = rect_bytes([0, 0, 1, 1]);
        let real = {
            let mut v = vec![0u8, 255]; // real flags, real user mask background
            v.extend(rect_bytes([0, 0, 2, 2]));
            v
        };
        let parameters = [0x01u8, 200]; // parameter flags (user density), density
        let build = |real_first: bool| {
            let mut m = user.clone();
            m.extend([0, 0x11]); // default colour, flags: bit 0 relative, bit 4 parameters present
            if real_first {
                m.extend(&real);
                m.extend(parameters);
            } else {
                m.extend(parameters);
                m.extend(&real);
            }
            m
        };
        let mut params_only = user.clone();
        params_only.extend([0, 0x11, 0x01, 200]);
        for (name, mask, real_flags_at) in [("spec order", build(false), Some(20)), ("photoshop order", build(true), Some(18)), ("no real fields", params_only, None)] {
            let mut l = TLayer::new("m", [0, 0, 1, 1]);
            l.channels = vec![(0, 0, vec![1]), (1, 0, vec![2]), (2, 0, vec![3]), (-2, 0, vec![10]), (-3, 0, vec![1, 2, 3, 4])];
            l.mask = mask;
            let info = layer_info(&[l], false, false);
            let doc = read(&minimal(layer_section(&info, &[], false), Some((0, vec![1, 2, 3]))).bytes());
            let layer = &doc.layers()[0];
            let view = layer.mask().expect("mask view");
            assert_eq!(view.real_flags_offset, real_flags_at, "{name}");
            if real_flags_at.is_some() {
                assert_eq!(view.real_rect.map(|r| (r.right, r.bottom)), Some((2, 2)), "{name}");
                assert_eq!(view.real_default_color, Some(255), "{name}");
                assert_eq!((layer.channel_rect(-3).width(), layer.channel_rect(-3).height()), (2, 2), "{name}");
                let plane = layer.decode_channel(-3, &doc.header, &limits, &cancel).unwrap().unwrap();
                assert_eq!((plane.width, plane.height, plane.bytes), (2, 2, vec![1, 2, 3, 4]), "{name}");
            } else {
                assert!(view.real_rect.is_none(), "{name}");
            }
            assert_eq!(layer.channel_rect(-2).width(), 1, "{name}: the user mask keeps its own rectangle");
        }
    });
}

/// Finding 4: the model manual no longer lists shipped work as pending and is valid JSON.
#[test]
fn descriptor_is_valid_json_and_does_not_list_shipped_work_as_pending() {
    bounded_case("descriptor_is_valid_json_and_does_not_list_shipped_work_as_pending", || {
        let d: serde_json::Value = serde_json::from_str(DESCRIPTOR).expect("DESCRIPTOR is JSON");
        let pending: Vec<&str> = d["pending"].as_array().unwrap().iter().filter_map(|v| v.as_str()).collect();
        assert!(!pending.contains(&"proposal_mapping"), "to_proposal ships");
        assert!(!pending.iter().any(|p| p.contains("SMB-PRISM-ENG-3")), "the Prism n-channel path shipped; the dependency is no longer pending");
        assert!(pending.iter().any(|p| p.contains("inverted_storage")), "unverified CMYK/Lab encodings are recorded as pending verification");
        let caps = d["capabilities"].to_string();
        assert!(caps.contains("folio_proposal") && caps.contains("pigment_tiles_8_16_32_bit") && caps.contains("cmyk_embedded_icc_display_conversion"));
        assert!(d["api"].as_str().unwrap().contains("to_proposal"));
        assert_eq!(d["limits"]["max_layers"], 8192);
    });
}

// ---- R5 review (carried PLAUSIBLE items) -------------------------------------------------------

/// A `luni` block the reader cannot take (over `max_name_units`) leaves the Pascal name as the display
/// name. That is a typed loss, the block survives a rewrite byte for byte, and a readable `luni` is silent.
#[test]
fn unreadable_unicode_layer_name_is_a_typed_loss() {
    bounded_case("unreadable_unicode_layer_name_is_a_typed_loss", || {
        let limits = Limits::default();
        let layer_with = |luni: &str| {
            let mut l = TLayer::new("pascal", [0, 0, 1, 1]);
            l.channels = vec![(0, 0, vec![1]), (1, 0, vec![2]), (2, 0, vec![3])];
            l.blocks = vec![layer_block(b"luni", &unicode(luni), false)];
            let info = layer_info(&[l], false, false);
            minimal(layer_section(&info, &[], false), Some((0, vec![1, 2, 3]))).bytes()
        };
        let too_long = layer_with(&"u".repeat(limits.max_name_units + 1));
        let doc = read(&too_long);
        assert_eq!(doc.layers()[0].name, "pascal", "the Pascal name stands in");
        let report = doc.loss_report(&limits);
        assert!(
            report
                .entries
                .iter()
                .any(|e| e.path == "layer[0]/name" && e.code == "layer_name_unicode_unreadable" && e.kind == LossKind::Transformed),
            "{}",
            report.to_json()
        );
        assert_eq!(write_psd(&doc, &limits).unwrap(), too_long, "the unreadable block is preserved");

        let fine = read(&layer_with("readable"));
        assert_eq!(fine.layers()[0].name, "readable");
        assert!(!fine.loss_report(&limits).has_code("layer_name_unicode_unreadable"));
    });
}

/// The 28 PSD blend keys against a literal copy of the specification's key list (Adobe Photoshop File
/// Format, layer record blend mode key; verified list in research/adobe-behavior.md), written separately
/// from `PSD_BLEND_KEYS` so a wrong row there cannot pass by agreeing with itself.
#[test]
fn psd_blend_key_rows_match_the_specification_literally() {
    bounded_case("psd_blend_key_rows_match_the_specification_literally", || {
        let rows: [(&[u8; 4], Pigment); 28] = [
            (b"pass", Pigment::PassThrough),
            (b"norm", Pigment::Normal),
            (b"diss", Pigment::Dissolve),
            (b"dark", Pigment::Darken),
            (b"mul ", Pigment::Multiply),
            (b"idiv", Pigment::ColourBurn),
            (b"lbrn", Pigment::LinearBurn),
            (b"dkCl", Pigment::DarkerColour),
            (b"lite", Pigment::Lighten),
            (b"scrn", Pigment::Screen),
            (b"div ", Pigment::ColourDodge),
            (b"lddg", Pigment::LinearDodgeAdd),
            (b"lgCl", Pigment::LighterColour),
            (b"over", Pigment::Overlay),
            (b"sLit", Pigment::SoftLight),
            (b"hLit", Pigment::HardLight),
            (b"vLit", Pigment::VividLight),
            (b"lLit", Pigment::LinearLight),
            (b"pLit", Pigment::PinLight),
            (b"hMix", Pigment::HardMix),
            (b"diff", Pigment::Difference),
            (b"smud", Pigment::Exclusion),
            (b"fsub", Pigment::Subtract),
            (b"fdiv", Pigment::Divide),
            (b"hue ", Pigment::Hue),
            (b"sat ", Pigment::Saturation),
            (b"colr", Pigment::Colour),
            (b"lum ", Pigment::Luminosity),
        ];
        for (key, want) in rows {
            assert_eq!(studio_blend_for_key(*key), Some(want), "{}", key_text(*key));
            assert_eq!(psd_key_for(want), Some(*key), "{}", key_text(*key));
        }
        assert_eq!(PSD_BLEND_KEYS.len(), rows.len());
    });
}
