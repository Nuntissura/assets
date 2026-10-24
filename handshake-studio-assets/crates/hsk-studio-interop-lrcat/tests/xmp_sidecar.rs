//! Lane SMB-LR-LIBRARY-WRITE slice 2: XMP sidecar read/write for develop settings (crs:) and
//! library fields, byte-wise preservation of unknown content, hostile packets, and the write path
//! through the CKC port (test-only double). The packet below is hand-written in the shape of a
//! Lightroom Classic sidecar; Camera Raw names and formats are UNVERIFIED (D5).
use hsk_studio_accord::DomainId;
use hsk_studio_develop::{DevelopRecipe, ProfileBinding, VignetteStyle};
use hsk_studio_interop_lrcat::{
    LibrarySidecar, SidecarUpdate, WhiteBalanceSource, XmpLimits, XmpRating, read_sidecar, read_sidecar_via_port, write_sidecar,
    write_sidecar_via_port,
};
use hsk_studio_kernel_bindings::{ActorKind, AssetFacts, AssetId, CkcActor, CkcError, CkcLimits, CkcTestDouble, SidecarPlacement};

fn recipe() -> DevelopRecipe {
    let profile = ProfileBinding { profile_id: DomainId::parse("SCPF-019abcde-0000-7000-8000-000000000001").unwrap(), sha256: [7; 32] };
    DevelopRecipe::native([1.0; 3], [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0], profile)
}

fn text_has_crs_decl(b: &[u8]) -> bool {
    String::from_utf8_lossy(b).contains("xmlns:crs=\"http://ns.adobe.com/camera-raw-settings/1.0/\"")
}

fn lim() -> XmpLimits {
    XmpLimits::default()
}

/// Lightroom-shaped sidecar: attribute and element forms, unknown namespaces, a tone-curve
/// sequence and a nested mask structure (crs: but unmanaged), comments, CDATA, single quotes.
const SIDECAR: &str = r#"<?xpacket begin="﻿" id="W5M0MpCehiHzreSzNTczkc9d"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/" x:xmptk="Adobe XMP Core 7.0-c000 1.000000, 0000/00/00-00:00:00        ">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:xmp="http://ns.adobe.com/xap/1.0/"
    xmlns:aux="http://ns.adobe.com/exif/1.0/aux/"
    xmlns:camera="http://ns.adobe.com/camera-raw-settings/1.0/"
    xmlns:dc="http://purl.org/dc/elements/1.1/"
    xmlns:vendor="urn:example:vendor"
   xmp:Rating="3"
   xmp:Label='Red'
   aux:Lens="24-70mm F2.8 &amp; more"
   camera:Version="15.0"
   camera:ProcessVersion="11.0"
   camera:WhiteBalance="As Shot"
   camera:Temperature="5200"
   camera:Exposure2012="+0.50"
   camera:Contrast2012="+12"
   camera:Sharpness="40"
   camera:LensProfileEnable="1"
   camera:HasCrop="False"
   vendor:Opaque="keep &lt;me&gt; exactly">
   <!-- vendor comment: must survive -->
   <camera:Clarity2012>+8</camera:Clarity2012>
   <camera:ToneCurvePV2012>
    <rdf:Seq>
     <rdf:li>0, 0</rdf:li>
     <rdf:li>255, 255</rdf:li>
    </rdf:Seq>
   </camera:ToneCurvePV2012>
   <camera:MaskGroupBasedCorrections>
    <rdf:Seq>
     <rdf:li rdf:parseType="Resource">
      <camera:What>Correction</camera:What>
      <camera:CorrectionMasks><rdf:Seq><rdf:li camera:What="Mask/Gradient" camera:ZeroX="0.5"/></rdf:Seq></camera:CorrectionMasks>
     </rdf:li>
    </rdf:Seq>
   </camera:MaskGroupBasedCorrections>
   <vendor:Note><![CDATA[raw <cdata> & text]]></vendor:Note>
   <dc:subject>
    <rdf:Bag>
     <rdf:li>Rome</rdf:li>
     <rdf:li>Trip</rdf:li>
    </rdf:Bag>
   </dc:subject>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>
<?xpacket end="w"?>"#;

const UNKNOWN_BLOCKS: [&str; 6] = [
    "aux:Lens=\"24-70mm F2.8 &amp; more\"",
    "vendor:Opaque=\"keep &lt;me&gt; exactly\"",
    "<!-- vendor comment: must survive -->",
    "<camera:ToneCurvePV2012>\n    <rdf:Seq>\n     <rdf:li>0, 0</rdf:li>\n     <rdf:li>255, 255</rdf:li>\n    </rdf:Seq>\n   </camera:ToneCurvePV2012>",
    "<camera:CorrectionMasks><rdf:Seq><rdf:li camera:What=\"Mask/Gradient\" camera:ZeroX=\"0.5\"/></rdf:Seq></camera:CorrectionMasks>",
    "<vendor:Note><![CDATA[raw <cdata> & text]]></vendor:Note>",
];

#[test]
fn sidecar_roundtrip_of_crs_and_library_fields() {
    // A recipe with values in many managed groups, written to a fresh packet and read back.
    let mut r = recipe();
    r.exposure_stops = -1.25;
    r.adjust.tone.contrast = 20.0;
    r.adjust.tone.blacks = -15.0;
    r.adjust.presence.vibrance = 30.0;
    r.adjust.detail.texture = 10.0;
    r.adjust.detail.sharpen.radius = 1.5;
    r.adjust.mixer.bands[5].saturation = -40.0;
    r.adjust.grayscale.bands[0] = 25.0;
    r.adjust.grading.midtones.hue = 210.0;
    r.adjust.grading.balance = -20.0;
    r.adjust.parametric.darks = -10.0;
    r.adjust.effects.grain.amount = 35.0;
    r.adjust.effects.grain.seed = 4_000_000_000;
    r.adjust.effects.vignette.style = VignetteStyle::ColourPriority;
    r.adjust.calibration.red_hue = 12.0;
    r.adjust.geometry.perspective_rotate = -2.5;
    r.crop.left = 0.125;
    r.crop.right = 0.875;
    r.crop.angle = 3.25;
    r.enables.enable_detail = true;
    r.enables.enable_colour_adjustments = true;
    r.enables.enable_grayscale_mix = true;
    r.enables.enable_tone_curve = true;
    r.enables.enable_effects = true;
    r.enables.enable_calibration = true;
    r.enables.enable_transform = true;
    let lib = LibrarySidecar {
        rating: Some(XmpRating::Rejected),
        label: Some("Purple & <friends>".into()),
        keywords: vec!["Rome".into(), "Trip".into()],
        hierarchical_keywords: vec![vec!["Places".into(), "Italy".into(), "Rome".into()]],
    };
    let bytes = write_sidecar(None, &SidecarUpdate { recipe: Some(&r), library: Some(&lib) }, &lim()).unwrap();
    let back = read_sidecar(&bytes, &recipe(), &lim()).unwrap();
    assert_eq!(back.recipe, r, "every managed value survives write + read");
    assert_eq!(back.library, lib);
    assert_eq!((back.white_balance, back.losses.len(), back.preserved_properties), (WhiteBalanceSource::AsShot, 0, 0));
    // Writing the read result again changes nothing (byte-identical), and so does writing to it.
    let again = write_sidecar(Some(&bytes), &SidecarUpdate { recipe: Some(&back.recipe), library: Some(&back.library) }, &lim()).unwrap();
    assert_eq!(again, bytes);
    // Defaults are not written into a fresh packet: an identity, as-shot recipe yields no crs:
    // property at all (a reader infers the same values).
    let plain = String::from_utf8(write_sidecar(None, &SidecarUpdate { recipe: Some(&recipe()), library: None }, &lim()).unwrap()).unwrap();
    assert!(!plain.contains("crs:") && plain.contains("<rdf:Description rdf:about=\"\">"), "{plain}");
    assert!(text_has_crs_decl(&bytes));
    // Absolute white balance is written as Custom and reported (not applied) on read.
    let mut wb = r.clone();
    wb.adjust.absolute_white_balance = Some(hsk_studio_develop::AbsoluteWhiteBalance { kelvin: 4800.0, tint: 7.0 });
    wb.white_balance_mode = hsk_studio_develop::WbMode::Temperature;
    let text = String::from_utf8(write_sidecar(None, &SidecarUpdate { recipe: Some(&wb), library: None }, &lim()).unwrap()).unwrap();
    assert!(text.contains("crs:WhiteBalance=\"Custom\"") && text.contains("crs:Temperature=\"4800\"") && text.contains("crs:Tint=\"+7\""));
    let read = read_sidecar(text.as_bytes(), &recipe(), &lim()).unwrap();
    assert!(matches!(read.white_balance, WhiteBalanceSource::Absolute(a) if a.kelvin == 4800.0 && a.tint == 7.0));
    assert_eq!(read.recipe.adjust.absolute_white_balance, None, "needs the raw to apply");
}

#[test]
fn edits_touch_only_managed_spans_and_keep_unknown_bytes() {
    let read = read_sidecar(SIDECAR.as_bytes(), &recipe(), &lim()).unwrap();
    // Element and attribute forms read; the gated groups with authored values are enabled.
    assert_eq!((read.recipe.exposure_stops, read.recipe.adjust.tone.contrast, read.recipe.adjust.presence.clarity), (0.5, 12.0, 8.0));
    assert_eq!(read.recipe.adjust.detail.sharpen.amount, 40.0);
    assert!(read.recipe.enables.enable_detail && read.recipe.adjust.optics.lens_profile_enable);
    assert_eq!(read.library.rating, Some(XmpRating::Stars(3)));
    assert_eq!((read.library.label.as_deref(), read.library.keywords.len()), (Some("Red"), 2));
    // Unknown: aux:Lens, vendor:Opaque, crs Version/ProcessVersion, ToneCurvePV2012, mask groups, vendor:Note.
    assert_eq!(read.preserved_properties, 7);
    // No-op write: identical bytes.
    let same = write_sidecar(Some(SIDECAR.as_bytes()), &SidecarUpdate { recipe: Some(&read.recipe), library: Some(&read.library) }, &lim()).unwrap();
    assert_eq!(same, SIDECAR.as_bytes());
    // Edit: exposure (attribute), clarity (element form), a key absent before (Vibrance), rating,
    // label removed, keyword added, a hierarchical keyword in a namespace not declared yet (lr:).
    let mut r = read.recipe.clone();
    r.exposure_stops = 1.0;
    r.adjust.presence.clarity = -5.0;
    r.adjust.presence.vibrance = 15.0;
    let lib = LibrarySidecar {
        rating: Some(XmpRating::Stars(5)),
        label: None,
        keywords: vec!["Rome".into(), "Trip".into(), "Night".into()],
        hierarchical_keywords: vec![vec!["Places".into(), "Rome".into()]],
    };
    let out = String::from_utf8(write_sidecar(Some(SIDECAR.as_bytes()), &SidecarUpdate { recipe: Some(&r), library: Some(&lib) }, &lim()).unwrap()).unwrap();
    for block in UNKNOWN_BLOCKS {
        assert_eq!(out.matches(block).count(), 1, "unknown content kept verbatim: {block}");
    }
    assert!(out.contains("camera:Exposure2012=\"+1.00\"") && out.contains("<camera:Clarity2012>-5</camera:Clarity2012>"));
    assert!(out.contains("camera:Vibrance=\"+15\""), "existing prefix reused for the crs namespace");
    assert!(out.contains("xmp:Rating=\"5\"") && !out.contains("xmp:Label"));
    assert!(out.contains("xmlns:lr=\"http://ns.adobe.com/lightroom/1.0/\"") && out.contains("<rdf:li>Places|Rome</rdf:li>"));
    // Bytes before the first edited span and after the last one are untouched.
    let first = SIDECAR.find("xmp:Rating=").unwrap();
    assert_eq!(&out[..first], &SIDECAR[..first]);
    let tail = SIDECAR.find("  </rdf:Description>").unwrap();
    assert!(out.ends_with(&SIDECAR[tail..]));
    let back = read_sidecar(out.as_bytes(), &recipe(), &lim()).unwrap();
    assert_eq!((back.recipe.exposure_stops, back.recipe.adjust.presence.clarity, back.recipe.adjust.presence.vibrance), (1.0, -5.0, 15.0));
    assert_eq!(back.library, lib);
    assert_eq!(back.preserved_properties, 7);
    // Self-closing description: children force an open/close pair.
    let tiny = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description rdf:about="" xmlns:q="urn:q" q:k="v"/></rdf:RDF></x:xmpmeta>"#;
    let lib2 = LibrarySidecar { keywords: vec!["A".into()], ..LibrarySidecar::default() };
    let t = String::from_utf8(write_sidecar(Some(tiny.as_bytes()), &SidecarUpdate { recipe: None, library: Some(&lib2) }, &lim()).unwrap()).unwrap();
    assert!(t.contains("q:k=\"v\"") && t.contains("</rdf:Description>"));
    assert_eq!(read_sidecar(t.as_bytes(), &recipe(), &lim()).unwrap().library.keywords, ["A"]);
}

#[test]
fn hostile_sidecars_are_refused_typed() {
    let l = lim();
    let wrap = |inner: &str| {
        format!(r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description rdf:about="" xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/" xmlns:dc="http://purl.org/dc/elements/1.1/"{inner}</rdf:Description></rdf:RDF></x:xmpmeta>"#)
    };
    let deep = format!("<rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">{}{}</rdf:RDF>", "<a>".repeat(100), "</a>".repeat(100));
    let many_keywords = wrap(&format!("><dc:subject><rdf:Bag>{}</rdf:Bag></dc:subject>", "<rdf:li>k</rdf:li>".repeat(5000)));
    let cases: Vec<(Vec<u8>, &str)> = vec![
        (b"<!DOCTYPE x [<!ENTITY a \"b\">]><x/>".to_vec(), "xmp_doctype_refused"),
        (wrap(">").replace("</x:xmpmeta>", "").into_bytes(), "xmp_unclosed_element"),
        (wrap("></crs:Oops>").into_bytes(), "xmp_tag_mismatch"),
        (deep.into_bytes(), "xmp_depth_exceeded"),
        (many_keywords.into_bytes(), "xmp_keywords_exceeded"),
        (wrap(" crs:Exposure2012=\"1\"><crs:Exposure2012>2</crs:Exposure2012>").into_bytes(), "xmp_property_repeated"),
        (wrap(" crs:A=\"1\" crs:A=\"2\">").into_bytes(), "xmp_attribute_repeated"),
        (wrap(" zz:Exposure2012=\"1\">").into_bytes(), "xmp_unbound_prefix"),
        (wrap(" crs:Exposure2012=\"<1\">").into_bytes(), "xmp_attribute_value"),
        (wrap(" crs:Unmanaged=\"&bogus;\">").into_bytes(), "xmp_bad_reference"),
        (vec![0x3c, 0xff, 0xfe], "xmp_not_utf8"),
        (vec![b' '; 5 * 1024 * 1024], "xmp_input_too_large"),
        ([b"<r>".to_vec(), b"<a/>".repeat(70_000), b"</r>".to_vec()].concat(), "xmp_nodes_exceeded"),
    ];
    for (bytes, code) in &cases {
        let got = read_sidecar(bytes, &recipe(), &l).unwrap_err().code();
        assert_eq!(got, *code, "{}", String::from_utf8_lossy(&bytes[..bytes.len().min(80)]));
    }
    // Hostile VALUES are typed losses on read, never clamped or applied.
    let bad = wrap(" crs:Exposure2012=\"+9.0\" crs:Contrast2012=\"NaN\" crs:Sharpness=\"lots\" crs:PostCropVignetteStyle=\"3\" crs:EnableDetail=\"maybe\" crs:WhiteBalance=\"Daylight\">");
    let read = read_sidecar(bad.as_bytes(), &recipe(), &l).unwrap();
    let codes: Vec<&str> = read.losses.iter().map(|(_, c)| *c).collect();
    assert_eq!(codes, ["crs_value_out_of_range", "crs_value_out_of_range", "crs_value_not_number", "crs_vignette_style_unsupported", "crs_value_not_boolean", "crs_white_balance_preset_unmapped"]);
    assert_eq!(read.recipe, recipe(), "nothing applied");
    // Writes refuse what cannot be represented, before producing bytes.
    let w = |lib: LibrarySidecar| write_sidecar(None, &SidecarUpdate { recipe: None, library: Some(&lib) }, &l).unwrap_err().code();
    assert_eq!(w(LibrarySidecar { hierarchical_keywords: vec![vec!["a|b".into()]], ..Default::default() }), "xmp_keyword_separator");
    assert_eq!(w(LibrarySidecar { keywords: vec!["k".into(); 5000], ..Default::default() }), "xmp_keywords_exceeded");
    assert_eq!(w(LibrarySidecar { label: Some("a\u{0}b".into()), ..Default::default() }), "xmp_control_character");
    let mut nan = recipe();
    nan.adjust.tone.contrast = f64::NAN;
    assert_eq!(write_sidecar(None, &SidecarUpdate { recipe: Some(&nan), library: None }, &l).unwrap_err().code(), "crs_recipe_invalid");
    // A managed key in an unexpected shape is not overwritten blindly.
    let shaped = wrap("><crs:Exposure2012><rdf:Seq><rdf:li>1</rdf:li></rdf:Seq></crs:Exposure2012>");
    let mut r = recipe();
    r.exposure_stops = 2.0;
    assert_eq!(write_sidecar(Some(shaped.as_bytes()), &SidecarUpdate { recipe: Some(&r), library: None }, &l).unwrap_err().code(), "xmp_property_shape");
}

#[test]
fn sidecar_writes_go_through_the_ckc_port_with_revision_cas() {
    let d = CkcTestDouble::new(CkcLimits::default());
    let actor = CkcActor { kind: ActorKind::Operator, principal_id: "op-1".into() };
    let asset = AssetId::from_bytes([3; 16]);
    d.seed_asset(asset.clone(), AssetFacts::default()).unwrap();
    // No sidecar yet: a write creates one (expected revision None).
    assert_eq!(read_sidecar_via_port(&d, &actor, &asset, &recipe(), &lim()).unwrap(), None);
    let mut r = recipe();
    r.adjust.tone.shadows = 25.0;
    let up = SidecarUpdate { recipe: Some(&r), library: None };
    let rc = write_sidecar_via_port(&d, &actor, &asset, &up, SidecarPlacement::BesideOriginal, "x1", &lim()).unwrap().unwrap();
    assert_eq!((rc.command_id, rc.revision), ("asset.write_xmp_sidecar_bytes", 1));
    let (read, rev) = read_sidecar_via_port(&d, &actor, &asset, &recipe(), &lim()).unwrap().unwrap();
    assert_eq!((read.recipe.adjust.tone.shadows, rev), (25.0, 1));
    // Unchanged bytes: no command, no event.
    let events = d.events();
    assert_eq!(write_sidecar_via_port(&d, &actor, &asset, &up, SidecarPlacement::BesideOriginal, "x2", &lim()).unwrap(), None);
    assert_eq!(d.events(), events);
    // Seeded Lightroom-shaped sidecar: an edit keeps its unknown bytes in what CKC stores.
    let other = AssetId::from_bytes([4; 16]);
    d.seed_asset(other.clone(), AssetFacts::default()).unwrap();
    d.seed_sidecar(other.clone(), SIDECAR.as_bytes().to_vec()).unwrap();
    let (base, rev) = read_sidecar_via_port(&d, &actor, &other, &recipe(), &lim()).unwrap().unwrap();
    let mut edited = base.recipe.clone();
    edited.adjust.tone.contrast = -30.0;
    let rc = write_sidecar_via_port(&d, &actor, &other, &SidecarUpdate { recipe: Some(&edited), library: None }, SidecarPlacement::BesideOriginal, "x3", &lim()).unwrap().unwrap();
    assert_eq!((rev, rc.revision), (1, 2));
    // A stale precondition (someone rewrote the sidecar between read and write) is refused by the
    // port contract: replaying the request with the old revision fails.
    let stale = hsk_studio_kernel_bindings::XmpSidecarBytesWrite {
        idempotency_key: "x4".into(),
        asset_id: other.clone(),
        expected_sidecar_revision: Some(1),
        bytes: SIDECAR.as_bytes().to_vec(),
        placement: SidecarPlacement::BesideOriginal,
    };
    use hsk_studio_kernel_bindings::CkcCatalogPort;
    assert_eq!(d.write_xmp_sidecar_bytes(&actor, &stale).unwrap_err(), CkcError::StaleRevision { current: 2 });
    let creating = hsk_studio_kernel_bindings::XmpSidecarBytesWrite { idempotency_key: "x5".into(), expected_sidecar_revision: None, ..stale.clone() };
    assert_eq!(d.write_xmp_sidecar_bytes(&actor, &creating).unwrap_err(), CkcError::StaleRevision { current: 2 });
    let huge = hsk_studio_kernel_bindings::XmpSidecarBytesWrite { idempotency_key: "x6".into(), bytes: vec![b' '; 5 << 20], expected_sidecar_revision: Some(2), ..stale };
    assert_eq!(d.write_xmp_sidecar_bytes(&actor, &huge).unwrap_err().code(), "ckc_sidecar_bytes");
    // Denied actor: the read itself is unauthorized (no existence leak).
    d.deny("op-1").unwrap();
    assert_eq!(read_sidecar_via_port(&d, &actor, &other, &recipe(), &lim()).unwrap_err().code(), "ckc_unauthorized");
}

/// Lane SMB-LR-HEALING-1 (STU-RAW-103 decode of STU-RAW-124): `crs:RetouchAreas` structures (a nested
/// `rdf:Description`, `rdf:parseType="Resource"` children, `rdf:li` attributes) decode into develop
/// retouch records; unknown fields are loss notes, unusable areas are dropped with a loss note.
/// Camera Raw field names UNVERIFIED (D5).
#[test]
fn xmp_retouch_areas_decode_into_records() {
    use hsk_studio_develop::{Brush, RetouchArea, RetouchMethod, SourceState, SpotType};
    let packet = |areas: &str| {
        format!(
            r#"<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about="" xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/" crs:EnableRetouch="True">
   <crs:RetouchAreas>{areas}</crs:RetouchAreas>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#
        )
    };
    let explicit = |extra: &str| {
        format!(
            r#"<rdf:li><rdf:Description crs:SpotType="heal" crs:SourceState="sourceSetExplicitly" crs:SourceX="0.5" crs:SourceY="0.5" crs:CenterX="0.25" crs:CenterY="0.25" crs:Radius="0.05" {extra}/></rdf:li>"#
        )
    };
    let areas = format!(
        r#"<rdf:Seq>
     <rdf:li>
      <rdf:Description crs:SpotType="heal" crs:HealVersion="2" crs:Feather="0.4" crs:Opacity="0.9" crs:Seed="17" crs:Method="gaussian"
        crs:SourceState="sourceSetExplicitly" crs:SourceX="0.6" crs:SourceY="0.7" crs:OffsetY="0.01" crs:CenterX="0.2" crs:CenterY="0.3"
        crs:Radius="0.05" crs:Vendor="x">
       <crs:Masks>
        <rdf:Seq>
         <rdf:li crs:Radius="0.04" crs:Flow="0.8" crs:Feather="0.5" crs:What="Mask/Paint">
          <crs:Dabs><rdf:Seq><rdf:li>d 0.2 0.3</rdf:li><rdf:li>d 0.25 0.3</rdf:li></rdf:Seq></crs:Dabs>
         </rdf:li>
        </rdf:Seq>
       </crs:Masks>
      </rdf:Description>
     </rdf:li>
     <rdf:li rdf:parseType="Resource">
      <crs:SpotType>clone</crs:SpotType>
      <crs:SourceState>source_set_explicitly</crs:SourceState>
      <crs:SourceX>0.1</crs:SourceX>
      <crs:SourceY>0.125</crs:SourceY>
      <crs:CentreX>0.3</crs:CentreX>
      <crs:CentreY>0.35</crs:CentreY>
      <crs:Radius>0.02</crs:Radius>
     </rdf:li>
     <rdf:li crs:SpotType="patch" crs:SourceState="sourceSetExplicitly" crs:SourceX="0.5" crs:SourceY="0.5" crs:CenterX="0.2" crs:CenterY="0.2" crs:Radius="0.05"/>
     <rdf:li crs:SpotType="heal" crs:SourceState="sourceSetExplicitly" crs:SourceY="0.5" crs:CenterX="0.2" crs:CenterY="0.2" crs:Radius="0.05"/>
     {}
     {}
    </rdf:Seq>"#,
        explicit(r#"crs:Opacity="2""#),
        explicit(r#"crs:Seed="-3""#)
    );
    let read = read_sidecar(packet(&areas).as_bytes(), &recipe(), &lim()).unwrap();
    assert!(read.applied.contains(&"RetouchAreas"), "{:?}", read.applied);
    assert!(read.recipe.enables.enable_retouch);
    let got = &read.recipe.adjust.retouch.areas;
    assert_eq!(
        got,
        &vec![
            RetouchArea {
                spot_type: SpotType::Heal,
                heal_version: 2,
                feather: 0.4,
                opacity: 0.9,
                seed: 17,
                method: RetouchMethod::Gaussian,
                source_state: SourceState::SourceSetExplicitly,
                masks: vec![Brush { dabs: vec![(0.2, 0.3), (0.25, 0.3)], radius: 0.04, flow: 0.8, feather: 0.5 }],
                source_x: 0.6,
                source_y: 0.7,
                offset_y: 0.01,
                centre_x: 0.2,
                centre_y: 0.3,
                radius: 0.05,
            },
            RetouchArea::circle(SpotType::Clone, (0.3, 0.35), 0.02, (0.1, 0.125)),
        ]
    );
    for loss in [
        ("RetouchAreas[0].Vendor", "crs_retouch_field_unknown"),
        ("RetouchAreas[0].Masks[0].What", "crs_retouch_field_unknown"),
        ("RetouchAreas[2]", "crs_retouch_value_unmapped"),
        ("RetouchAreas[3]", "crs_retouch_field_missing"),
        ("RetouchAreas[4]", "crs_retouch_value_out_of_range"),
        ("RetouchAreas[5]", "crs_value_not_number"),
    ] {
        assert!(read.losses.contains(&(loss.0.to_string(), loss.1)), "{loss:?} in {:?}", read.losses);
    }
    // Nested descriptions are structure items, not top-level properties: two of them in one packet
    // decode (a repeated property refusal before this lane).
    let two = format!("<rdf:Seq>{}{}</rdf:Seq>", explicit(""), explicit(r#"crs:Seed="4""#));
    let read = read_sidecar(packet(&two).as_bytes(), &recipe(), &lim()).unwrap();
    assert_eq!(read.recipe.adjust.retouch.areas.len(), 2);
    assert_eq!(read.recipe.adjust.retouch.areas[1].seed, 4);
    // Hostile shapes: not a sequence, too many items; one loss, no areas, the read still succeeds.
    for (body, code) in [
        ("plain text".to_string(), "crs_retouch_shape"),
        (format!("<rdf:Seq>{}</rdf:Seq>", explicit("").repeat(hsk_studio_develop::retouch::MAX_RETOUCH_AREAS + 1)), "crs_retouch_too_many"),
    ] {
        let read = read_sidecar(packet(&body).as_bytes(), &recipe(), &lim()).unwrap();
        assert!(read.recipe.adjust.retouch.areas.is_empty());
        assert!(read.losses.contains(&("RetouchAreas".to_string(), code)), "{code}: {:?}", read.losses);
    }
}

// ---- lane SMB-LR-HEALING-2: crs:RetouchAreas write-back and circle masks (STU-RAW-103 encode) ----

fn retouch_recipe() -> DevelopRecipe {
    use hsk_studio_develop::{Brush, RetouchArea, RetouchMethod, SourceState, SpotType};
    let mut r = recipe();
    r.enables.enable_retouch = true;
    r.adjust.retouch.areas = vec![
        RetouchArea {
            spot_type: SpotType::Heal,
            heal_version: 3,
            feather: 0.35,
            opacity: 0.9,
            seed: u64::MAX - 7,
            method: RetouchMethod::Gaussian,
            source_state: SourceState::SourceSetExplicitly,
            masks: vec![Brush { dabs: vec![(0.1, 0.2), (0.125, 0.2)], radius: 0.03, flow: 0.8, feather: 0.5 }],
            source_x: 0.6,
            source_y: 0.7,
            offset_y: -0.015625,
            centre_x: 0.1,
            centre_y: 0.2,
            radius: 0.03,
        },
        RetouchArea { source_state: SourceState::EngineChosen, ..RetouchArea::circle(SpotType::Clone, (0.5, 0.5), 0.05, (0.5, 0.5)) },
        RetouchArea::circle(SpotType::Clone, (1.0 / 3.0, 0.7), 0.0625, (0.2, 0.1)),
    ];
    r
}

/// The recipe's retouch areas are written as a managed `crs:RetouchAreas` structure and read back
/// equal (every field, including non-dyadic numbers, the engine-chosen source and brush dabs), into
/// a fresh packet and into one that had a different list.
#[test]
fn retouch_areas_round_trip_through_write_and_read() {
    let r = retouch_recipe();
    let bytes = write_sidecar(None, &SidecarUpdate { recipe: Some(&r), library: None }, &lim()).unwrap();
    let text = String::from_utf8(bytes.clone()).unwrap();
    assert!(text.contains("<crs:RetouchAreas>") && text.contains("<crs:SourceState>engine_chosen</crs:SourceState>"), "{text}");
    let back = read_sidecar(&bytes, &recipe(), &lim()).unwrap();
    assert!(back.applied.contains(&"RetouchAreas"), "{:?}", back.losses);
    assert!(back.losses.iter().all(|(k, _)| !k.starts_with("RetouchAreas")), "{:?}", back.losses);
    assert_eq!(back.recipe.adjust.retouch.areas, r.adjust.retouch.areas);
    // Changing the list replaces only that structure: the bytes before and after it are unchanged.
    let mut fewer = r.clone();
    fewer.adjust.retouch.areas.remove(0);
    fewer.adjust.retouch.areas[1].opacity = 0.5;
    let changed = write_sidecar(Some(&bytes), &SidecarUpdate { recipe: Some(&fewer), library: None }, &lim()).unwrap();
    let again = read_sidecar(&changed, &recipe(), &lim()).unwrap();
    assert_eq!(again.recipe.adjust.retouch.areas, fewer.adjust.retouch.areas);
    let (start, end) = (text.find("<crs:RetouchAreas>").unwrap(), text.find("</crs:RetouchAreas>").unwrap() + "</crs:RetouchAreas>".len());
    let changed_text = String::from_utf8(changed.clone()).unwrap();
    assert!(changed_text.starts_with(&text[..start]) && changed_text.ends_with(&text[end..]), "{changed_text}");
    // An empty list removes the structure.
    let mut none = r.clone();
    none.adjust.retouch.areas.clear();
    let removed = String::from_utf8(write_sidecar(Some(&bytes), &SidecarUpdate { recipe: Some(&none), library: None }, &lim()).unwrap()).unwrap();
    assert!(!removed.contains("RetouchAreas"), "{removed}");
    assert!(read_sidecar(removed.as_bytes(), &recipe(), &lim()).unwrap().recipe.adjust.retouch.areas.is_empty());
}

/// A write whose retouch list equals the packet's decoded list leaves the packet bytes unchanged,
/// including another writer's spelling (attribute form, nested description, unknown fields).
#[test]
fn unchanged_retouch_list_leaves_the_packet_bytes() {
    let packet = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about="" xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/">
   <crs:RetouchAreas>
    <rdf:Seq>
     <rdf:li><rdf:Description crs:SpotType="heal" crs:SourceState="sourceSetExplicitly" crs:SourceX='0.5' crs:SourceY="0.50" crs:CenterX="0.25" crs:CenterY="0.25" crs:Radius="+0.05" crs:Vendor="kept"/></rdf:li>
    </rdf:Seq>
   </crs:RetouchAreas>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;
    let read = read_sidecar(packet.as_bytes(), &recipe(), &lim()).unwrap();
    assert_eq!(read.recipe.adjust.retouch.areas.len(), 1);
    let first = write_sidecar(Some(packet.as_bytes()), &SidecarUpdate { recipe: Some(&read.recipe), library: None }, &lim()).unwrap();
    let again = read_sidecar(&first, &recipe(), &lim()).unwrap();
    assert_eq!(again.recipe.adjust.retouch.areas, read.recipe.adjust.retouch.areas);
    let text = String::from_utf8(first).unwrap();
    let span = |t: &str| t[t.find("<crs:RetouchAreas>").unwrap()..t.find("</crs:RetouchAreas>").unwrap()].to_string();
    assert_eq!(span(&text), span(packet), "the retouch structure was rewritten");
    // A second no-op write is byte-identical to its input.
    let second = write_sidecar(Some(text.as_bytes()), &SidecarUpdate { recipe: Some(&again.recipe), library: None }, &lim()).unwrap();
    assert_eq!(second, text.as_bytes());
}

/// Circle retouch areas decode: an area without masks (centre and radius) and a `Mask/Circle`
/// mask item (one dab at its centre, or the area centre); a gradient mask is a named loss.
#[test]
fn circle_retouch_area_decodes() {
    use hsk_studio_develop::{Brush, RetouchArea, SpotType};
    let packet = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about="" xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/">
   <crs:RetouchAreas>
    <rdf:Seq>
     <rdf:li crs:SpotType="clone" crs:SourceState="sourceSetExplicitly" crs:SourceX="0.6" crs:SourceY="0.4" crs:CenterX="0.3" crs:CenterY="0.4" crs:Radius="0.04"/>
     <rdf:li rdf:parseType="Resource">
      <crs:SpotType>heal</crs:SpotType><crs:SourceState>sourceSetExplicitly</crs:SourceState>
      <crs:SourceX>0.7</crs:SourceX><crs:SourceY>0.7</crs:SourceY><crs:CenterX>0.2</crs:CenterX><crs:CenterY>0.25</crs:CenterY>
      <crs:Masks><rdf:Seq>
       <rdf:li crs:What="Mask/Circle" crs:CenterX="0.21" crs:CenterY="0.26" crs:Radius="0.03" crs:Feather="0.5" crs:Flow="0.75"/>
       <rdf:li crs:What="Mask/Circle" crs:Radius="0.02"/>
      </rdf:Seq></crs:Masks>
     </rdf:li>
     <rdf:li rdf:parseType="Resource">
      <crs:SpotType>heal</crs:SpotType><crs:SourceState>sourceSetExplicitly</crs:SourceState>
      <crs:SourceX>0.7</crs:SourceX><crs:SourceY>0.7</crs:SourceY><crs:CenterX>0.2</crs:CenterX><crs:CenterY>0.25</crs:CenterY>
      <crs:Masks><rdf:Seq><rdf:li crs:What="Mask/Gradient" crs:Radius="0.02"/></rdf:Seq></crs:Masks>
     </rdf:li>
    </rdf:Seq>
   </crs:RetouchAreas>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;
    let read = read_sidecar(packet.as_bytes(), &recipe(), &lim()).unwrap();
    let got = &read.recipe.adjust.retouch.areas;
    let mut masked = RetouchArea::circle(SpotType::Heal, (0.2, 0.25), 0.0, (0.7, 0.7));
    masked.masks = vec![
        Brush { dabs: vec![(0.21, 0.26)], radius: 0.03, flow: 0.75, feather: 0.5 },
        Brush { dabs: vec![(0.2, 0.25)], radius: 0.02, flow: 1.0, feather: 0.0 },
    ];
    assert_eq!(got, &vec![RetouchArea::circle(SpotType::Clone, (0.3, 0.4), 0.04, (0.6, 0.4)), masked], "{:?}", read.losses);
    assert!(read.losses.contains(&("RetouchAreas[2]".to_string(), "crs_retouch_mask_gradient")), "{:?}", read.losses);
    assert!(read.losses.iter().all(|(k, _)| !k.starts_with("RetouchAreas[1]") && !k.starts_with("RetouchAreas[0]")), "{:?}", read.losses);
    // The decoded circles write back and read back equal.
    let bytes = write_sidecar(None, &SidecarUpdate { recipe: Some(&read.recipe), library: None }, &lim()).unwrap();
    assert_eq!(&read_sidecar(&bytes, &recipe(), &lim()).unwrap().recipe.adjust.retouch.areas, got);
}
