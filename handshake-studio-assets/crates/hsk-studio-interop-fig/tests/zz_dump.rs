use hsk_studio_accord::{CancellationToken, SCHEMA_STUDIO_COMPONENT_INSTANCE};
use hsk_studio_folio::{Budget, Resolution, Resolver, TileRef};
use hsk_studio_interop_fig::*;
struct Host;
impl Resolver for Host {
    fn primitive(&self, _: &str, _: &str) -> Resolution { Resolution::Absent }
    fn profile(&self, _: &str) -> Resolution { Resolution::Absent }
    fn tile(&self, _: &TileRef) -> Resolution { Resolution::Absent }
    fn format_supported(&self, _: &str) -> bool { false }
    fn primitive_layer_binding(&self, kind: &str, schema: &str) -> bool {
        kind == "component_instance" && schema == SCHEMA_STUDIO_COMPONENT_INSTANCE
    }
}
#[test]
fn dump() {
    for name in ["fig-basic-shapes.fig", "fig-components-autolayout.fig", "fig-prototype-pages.fig"] {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name);
        let bytes = std::fs::read(path).unwrap();
        let cancel = CancellationToken::default();
        let FigOutcome::Imported(import) = import_fig(&bytes, &Limits::default(), &VersionPolicy::observed(), None, &cancel).unwrap() else { panic!() };
        let doc = decode_document(&import, &Limits::default(), &cancel).unwrap();
        let cfg = NativeConfig { budget: Budget::default(), resolver: &Host };
        let p = to_native(&doc, &cfg, &mut SequentialIdMint::new(0xF16), &cancel).unwrap();
        let d = p.snapshot.document();
        println!("=== {name}");
        for a in &d.artboards { println!("AB {} {} {:?} {:?}", a.artboard_id, a.name, a.parent, a.bounds); }
        for l in &d.layers { println!("L {} {} kind={} vis={} parent={:?} app={:?}", l.layer_id, l.name, l.kind, l.visible, l.parent, l.appearance.as_ref().map(|a| (a.opacity, a.blend_mode, a.isolation))); }
    }
}
