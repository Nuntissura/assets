//! EPSF 3.0 export (lane SMB-DXF-EPS-IMPORT): an imported SVG written as DSC-conforming EPS from the
//! same path model and Folio design records; the export reads back through the EPS DSC reader.
use hsk_studio_accord::CancellationToken;
use hsk_studio_interop_vector::*;

const SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="200" height="100" viewBox="0 0 200 100">
  <defs><linearGradient id="g" x1="0" y1="0" x2="1" y2="0"><stop offset="0" stop-color="#ff0000"/><stop offset="1" stop-color="#0000ff"/></linearGradient></defs>
  <rect x="10" y="10" width="80" height="40" fill="#00ff00" stroke="#000000" stroke-width="4" stroke-dasharray="6 2"/>
  <circle cx="150" cy="50" r="30" fill="url(#g)"/>
  <path d="M10 90 L190 90" stroke="#0000ff" stroke-opacity="0.5" fill="none"/>
  <g display="none"><rect x="0" y="0" width="5" height="5" fill="#ff00ff"/></g>
</svg>"##;

/// Solid fills, a dashed stroke, a linear gradient (`shfill`) and the artboard bounding box are
/// written; stroke opacity and the hidden group are typed losses; the output is deterministic and
/// its DSC header reads back with the artboard's box.
#[test]
fn proposal_exports_as_dsc_conforming_epsf() {
    let p = import_svg(SVG.as_bytes(), &ImportPolicy::default(), &mut SequentialIdMint::new(9), &CancellationToken::default()).expect("svg import");
    let out = export_eps_proposal(&p, &ExportOptions::default()).expect("eps export");
    let again = export_eps_proposal(&p, &ExportOptions::default()).expect("eps export again");
    assert_eq!(out, again, "deterministic");
    let t = &out.text;
    assert!(t.starts_with("%!PS-Adobe-3.0 EPSF-3.0\n%%BoundingBox: 0 0 150 75\n%%HiResBoundingBox: 0 0 150 75\n"), "{}", &t[..t.len().min(200)]);
    assert!(t.contains("%%EndComments\n") && t.trim_end().ends_with("%%EOF"));
    assert!(t.contains("0 1 0 rg fill"), "green rect fill");
    assert!(t.contains("[4.5 1.5] 0 d") && t.contains("0 0 0 RG S"), "dashed black stroke in points");
    assert!(t.contains("/ShadingType 2") && t.contains("shfill"), "gradient as a LanguageLevel 3 shading");
    assert!(!t.contains("1 0 1 rg"), "the hidden group's magenta rect is not drawn");
    assert!(out.loss.iter().any(|l| l.code == "opacity_not_exported"));
    assert!(out.loss.iter().any(|l| l.code == "hidden_layer_skipped"));
    assert!(out.loss.iter().any(|l| l.code == "eps_preview_not_written"));
    let info = read_eps(t.as_bytes(), &ImportPolicy::default()).expect("dsc reads back");
    assert_eq!(info.bounding_box, Some([0, 0, 150, 75]));
    assert_eq!(info.epsf_version.as_deref(), Some("EPSF-3.0"));
    assert_eq!(info.language_level, Some(3));
}
