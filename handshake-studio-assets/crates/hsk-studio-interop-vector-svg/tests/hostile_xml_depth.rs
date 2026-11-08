//! SMB-PARSER-HOSTILE-1 item 1 (red) and item 2 (guard): element nesting reaches roxmltree 0.21.1,
//! which parses nesting by recursion with no depth limit (`tokenizer.rs` `parse_element` /
//! `parse_content`); its node budget (20,000 here, `Limits::max_nodes`) is far above a depth that
//! overflows a thread stack, and a stack overflow aborts the process. Own test binary: an abort
//! takes the binary down, which counts as the test failing. Every case runs on an explicit thread
//! stack (`std::thread::Builder`), never the default.
//!
//! After item 2 the nesting scan (`hsk_studio_package::xml_nesting_within`, cap
//! `XML_MAX_NESTING` 256) runs before the parse and refuses deeper text with the same code the
//! walk gives past `Limits::max_depth` (32): `BudgetExceeded("depth")`.
use hsk_studio_accord::CancellationToken;
use hsk_studio_interop_vector_core::{ImportPolicy, SequentialIdMint, VectorError};
use hsk_studio_interop_vector_svg::import_svg;

const SVG_OPEN: &str = "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"10\" height=\"10\">";
/// Stack of the case threads (F-R56-1 measured on 2 MiB; Rust's default may change).
const CASE_STACK: usize = 2 << 20;

fn on_stack<T: Send + 'static>(stack: usize, f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(stack).spawn(f).expect("case thread").join().expect("case thread panicked")
}

fn import(text: String) -> Result<usize, VectorError> {
    on_stack(CASE_STACK, move || {
        import_svg(text.as_bytes(), &ImportPolicy::default(), &mut SequentialIdMint::new(1), &CancellationToken::default()).map(|p| p.loss.len())
    })
}

/// `<svg>` then `depth` `<g>` openers, closed when `closed`.
fn deep_svg(depth: usize, closed: bool) -> String {
    let mut s = String::with_capacity(depth.saturating_mul(7).saturating_add(128));
    s.push_str(SVG_OPEN);
    s.push_str(&"<g>".repeat(depth));
    if closed {
        s.push_str(&"</g>".repeat(depth));
        s.push_str("</svg>");
    }
    s
}

#[test]
fn svg_openers_are_a_typed_depth_refusal() {
    // 19,990 openers + the root: 19,991 nodes, inside the 20,000 node budget.
    assert_eq!(import(deep_svg(19_990, false)), Err(VectorError::BudgetExceeded("depth")));
}

#[test]
fn svg_closed_is_a_typed_depth_refusal() {
    // Closed, the text is 19,991 elements and 1 text-free root: inside the node budget.
    assert_eq!(import(deep_svg(19_990, true)), Err(VectorError::BudgetExceeded("depth")));
}

#[test]
fn svg_within_cap_keeps_the_walk_answer() {
    // 200 levels: under the scan's 256, past the walk's 32. The answer is the walk's own refusal,
    // as before the scan existed.
    assert_eq!(import(deep_svg(200, true)), Err(VectorError::BudgetExceeded("depth")));
    // 20 levels: under both; imports.
    assert!(import(deep_svg(20, true)).is_ok());
}

// ----- item 2: the scan against roxmltree's own tree ---------------------------------------------

/// xorshift64* (fixed seed: no clock, no environment).
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
}

/// A random well-formed document: elements, empty elements, text, comments, CDATA, processing
/// instructions and attribute values holding `<`-free decoys (`>`, `/>`, quotes of the other kind).
fn random_doc(rng: &mut Rng, max_depth: u64) -> String {
    fn element(rng: &mut Rng, out: &mut String, depth: u64, max_depth: u64) {
        let attrs = match rng.below(4) {
            0 => "",
            1 => " a=\"x/>\"",
            2 => " b='>' c=\"'\"",
            _ => " d = \"1\"\n",
        };
        if depth >= max_depth || rng.below(5) == 0 {
            out.push_str(&format!("<e{attrs}/>"));
            return;
        }
        out.push_str(&format!("<e{attrs}>"));
        for _ in 0..rng.below(4) {
            match rng.below(6) {
                0 => out.push_str("text &amp; more"),
                1 => out.push_str("<!-- <g><g> -->"),
                2 => out.push_str("<![CDATA[<g><g></g>]]>"),
                3 => out.push_str("<?pi <g> ?>"),
                _ => element(rng, out, depth + 1, max_depth),
            }
        }
        out.push_str("</e >");
    }
    let mut out = String::from("<?xml version=\"1.0\"?><!-- head <x> --><r>");
    element(rng, &mut out, 2, max_depth);
    out.push_str("</r>");
    out
}

/// Deepest element of a parsed document, counted as the scan counts (root element = 1).
fn roxmltree_depth(text: &str) -> usize {
    let doc = roxmltree::Document::parse(text).expect("generated document parses");
    doc.descendants().filter(|n| n.is_element()).map(|n| n.ancestors().filter(|a| a.is_element()).count()).max().unwrap_or(0)
}

#[test]
fn scan_depth_equals_roxmltree_depth_on_valid_documents() {
    on_stack(CASE_STACK, || {
        let mut rng = Rng(0x5EED_0001_D1FF_0001);
        for i in 0..400u64 {
            let text = random_doc(&mut rng, 2 + i % 40);
            let d = roxmltree_depth(&text);
            assert!(hsk_studio_package::xml_nesting_within(&text, d), "doc {i}: depth {d} must be within {d}");
            assert!(!hsk_studio_package::xml_nesting_within(&text, d - 1), "doc {i}: depth {d} must exceed {}", d - 1);
        }
    });
}

#[test]
fn the_cap_parses_on_1_mib() {
    // XML_MAX_NESTING levels parse in roxmltree on a 1 MiB thread (the Windows default main-thread
    // reserve, `/STACK`; where the Viewer runs imports is NOT_INSPECTED), with the options
    // `import_svg` uses.
    let cap = hsk_studio_package::XML_MAX_NESTING;
    let text = format!("{SVG_OPEN}{}{}</svg>", "<g>".repeat(cap - 1), "</g>".repeat(cap - 1));
    assert!(hsk_studio_package::xml_nesting_within(&text, cap));
    let parsed = on_stack(1 << 20, move || {
        let options = roxmltree::ParsingOptions { allow_dtd: false, nodes_limit: 20_000, entity_resolver: None };
        roxmltree::Document::parse_with_options(&text, options).map(|d| d.descendants().count()).map_err(|e| e.to_string())
    });
    assert_eq!(parsed, Ok(cap + 1), "root node + {cap} elements");
    // The import runs the walk too (refused at max_depth 32, typed) on the same 1 MiB.
    let answer = on_stack(1 << 20, move || {
        let text = format!("{SVG_OPEN}{}{}</svg>", "<g>".repeat(cap - 1), "</g>".repeat(cap - 1));
        import_svg(text.as_bytes(), &ImportPolicy::default(), &mut SequentialIdMint::new(1), &CancellationToken::default()).map(|p| p.loss.len())
    });
    assert_eq!(answer, Err(VectorError::BudgetExceeded("depth")));
}

/// Measurement probe (item 1: the smallest depth that aborts on a 2 MiB thread). Not a gate:
/// `HOSTILE_PROBE_DEPTH=<n> cargo nextest run ... --run-ignored only svg_depth_probe` runs one
/// import of `n` closed levels; an abort at `n` and none at `n - 1` is the measurement.
#[test]
#[ignore = "measurement probe, run by hand with HOSTILE_PROBE_DEPTH"]
fn svg_depth_probe() {
    let Some(depth) = std::env::var("HOSTILE_PROBE_DEPTH").ok().and_then(|v| v.parse::<usize>().ok()) else {
        println!("NOT_PROVEN: HOSTILE_PROBE_DEPTH not set");
        return;
    };
    let answer = import(deep_svg(depth, true));
    println!("PROBE svg depth={depth} answer={answer:?}");
}
