//! SMB-PARSER-HOSTILE-1 item 6, SVG target: a deterministic mutation sweep
//! (`hsk-studio-interop-tiff/tests/support/hostile_mutator.rs`) over inline documents covering
//! gradients, clips, masks, patterns, markers, CSS and `use`, and one SVGZ, read by `import_svg`
//! with `ImportPolicy::default()` (the production bounds). Raw byte damage dies at UTF-8 and XML, so
//! the dictionary holds tag and attribute tokens and decimal numbers are swapped.
#[path = "../../hsk-studio-interop-tiff/tests/support/hostile_mutator.rs"]
mod hostile_mutator;

use hostile_mutator::*;
use hsk_studio_accord::CancellationToken;
use hsk_studio_interop_vector_core::{ImportPolicy, SequentialIdMint};
use hsk_studio_interop_vector_svg::{gzip_svg, import_svg};

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc;

const CAP: usize = 256 << 20;

const DOCS: [&str; 4] = [
    r##"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="200" height="120" viewBox="0 0 200 120">
<defs>
<linearGradient id="lg" x1="0" y1="0" x2="1" y2="0" gradientTransform="rotate(30)"><stop offset="0" stop-color="#f00"/><stop offset="0.5" stop-color="rgb(0,128,0)" stop-opacity="0.5"/><stop offset="1" stop-color="blue"/></linearGradient>
<radialGradient id="rg" cx="0.5" cy="0.5" r="0.5" fx="0.3" fy="0.3" spreadMethod="reflect" href="#lg"/>
<clipPath id="cp" clipPathUnits="objectBoundingBox"><rect x="0.1" y="0.1" width="0.8" height="0.8" rx="0.1"/></clipPath>
<mask id="mk" maskUnits="userSpaceOnUse" x="0" y="0" width="200" height="120"><circle cx="60" cy="60" r="40" fill="white"/></mask>
</defs>
<rect x="10" y="10" width="80" height="50" fill="url(#lg)" clip-path="url(#cp)"/>
<ellipse cx="140" cy="40" rx="40" ry="25" fill="url(#rg)" mask="url(#mk)" transform="translate(5 5) scale(0.9) skewX(5)"/>
<path d="M10 80 C 40 10, 65 10, 95 80 S 150 150, 180 80 Q 190 100 195 110 A 20 15 30 1 0 150 100 Z" fill="none" stroke="#333" stroke-width="2.5" stroke-dasharray="4 2" stroke-linejoin="round"/>
</svg>"##,
    r##"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="100" height="100">
<style>.a { fill: #0a0; stroke: black; } #b { opacity: 0.5 } rect.c:first-child { fill-opacity: .3 }</style>
<defs><pattern id="pt" width="10" height="10" patternUnits="userSpaceOnUse" patternTransform="rotate(45)"><rect width="5" height="5" fill="red"/></pattern>
<g id="tpl"><polygon points="0,0 10,0 5,8" class="a"/><polyline points="0,0 3,4 6,0"/></g>
<marker id="m" viewBox="0 0 10 10" refX="10" refY="5" markerWidth="6" markerHeight="6" orient="auto"><path d="M0,0 L10,5 L0,10 z"/></marker></defs>
<rect class="c" x="5" y="5" width="40" height="40" fill="url(#pt)"/>
<use xlink:href="#tpl" x="50" y="50"/><use href="#tpl" transform="matrix(1 0 0 1 60 10)"/>
<line id="b" x1="10" y1="90" x2="90" y2="90" stroke="#000" stroke-width="2" marker-end="url(#m)" style="stroke-linecap: round; display: inline"/>
<g opacity="0.8" fill-rule="evenodd" visibility="visible"><circle cx="70" cy="70" r="10" color="#123456" fill="currentColor"/></g>
</svg>"##,
    r##"<?xml version="1.0" encoding="UTF-8"?>
<!-- comment <g> -->
<svg xmlns="http://www.w3.org/2000/svg" width="10cm" height="5cm" viewBox="0 0 100 50" preserveAspectRatio="xMidYMid slice">
<a href="#x"><text x="5" y="20" font-family="Inter" font-size="12">Hi &amp; <tspan dy="2">there</tspan></text></a>
<g transform="rotate(10 50 25)"><rect width="1e3" height="0.0001" fill="none" stroke="red"/></g>
<switch><foreignObject width="10" height="10"/><circle r="3"/></switch>
</svg>"##,
    r##"<svg xmlns="http://www.w3.org/2000/svg"><g><g><g><path d="m1,1 h5 v5 h-5 z m10 10 l 2 2"/></g></g></g><image href="data:image/png;base64,iVBORw0KGgo=" width="1" height="1"/></svg>"##,
];

const PROFILE: Profile<'static> = Profile {
    dictionary: &[
        b"<g>", b"</g>", b"<svg", b"<use href=\"#tpl\"/>", b"url(#lg)", b"url(#cp)", b"url(#mk)", b"url(#pt)", b"url(#m)", b"href=\"#",
        b"transform=\"", b"d=\"M", b" A ", b" C ", b"stroke-width=\"", b"<!DOCTYPE svg [<!ENTITY e \"x\">]>", b"&e;", b"&#x10FFFF;",
        b"<![CDATA[", b"]]>", b"style=\"", b"@import", b"viewBox=\"", b"inf", b"NaN", b"e999", b"\xEF\xBB\xBF", b"\xC3",
    ],
    text: true,
};

fn answer(b: &[u8]) -> Answer {
    import_svg(b, &ImportPolicy::default(), &mut SequentialIdMint::new(1), &CancellationToken::default()).map(|_| ()).map_err(|e| e.code())
}

#[test]
fn svg_mutation_sweep() {
    let mut seeds: Vec<Vec<u8>> = DOCS.iter().map(|d| d.as_bytes().to_vec()).collect();
    seeds.push(gzip_svg(DOCS[0]));
    for s in &seeds {
        assert_eq!(answer(s), Ok(()), "seed imports");
    }
    let report = sweep("svg", 0x5763_0000_0000_0001, &seeds, 350, PROFILE, CAP, answer);
    report.assert_reach(44);
}
