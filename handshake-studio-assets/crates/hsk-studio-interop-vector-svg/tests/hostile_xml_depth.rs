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
