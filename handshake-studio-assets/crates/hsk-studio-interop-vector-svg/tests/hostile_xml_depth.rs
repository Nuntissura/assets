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

/// Caller thread stacks the within-cap rows run from (lead ruling 2026-11-08: the same answer in
/// debug and release from any caller thread; 1 MiB is the Windows default main-thread reserve).
const CALLERS: [usize; 2] = [1 << 20, 2 << 20];

fn import_from(stack: usize, text: String, policy: ImportPolicy) -> Result<usize, VectorError> {
    on_stack(stack, move || import_svg(text.as_bytes(), &policy, &mut SequentialIdMint::new(1), &CancellationToken::default()).map(|p| p.loss.len()))
}

#[test]
fn svg_within_cap_keeps_the_walk_answer() {
    for stack in CALLERS {
        // 200 and 256 levels: within the scan's cap, past the walk's 32: the walk's own refusal.
        for depth in [200usize, 256] {
            assert_eq!(import_from(stack, deep_svg(depth - 1, true), ImportPolicy::default()), Err(VectorError::BudgetExceeded("depth")), "{depth} levels from a {stack}-byte caller");
        }
        // 20 levels: under both; imports.
        assert!(import_from(stack, deep_svg(19, true), ImportPolicy::default()).is_ok(), "20 levels from a {stack}-byte caller");
        // 257 levels: refused by the scan before the parse.
        assert_eq!(import_from(stack, deep_svg(256, true), ImportPolicy::default()), Err(VectorError::BudgetExceeded("depth")));
    }
}

#[test]
fn a_caller_max_depth_past_the_cap_still_walks_the_cap_from_any_caller() {
    // `Limits::max_depth` is the caller's: at 300 the walk itself goes to the scan's 256 levels.
    let mut policy = ImportPolicy::default();
    policy.limits.max_depth = 300;
    policy.limits.max_nodes = 20_000;
    for stack in CALLERS {
        let r = import_from(stack, deep_svg(255, true), policy);
        assert!(r.is_ok(), "256 levels, max_depth 300, from a {stack}-byte caller: {r:?}");
        assert_eq!(import_from(stack, deep_svg(256, true), policy), Err(VectorError::BudgetExceeded("depth")));
    }
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
fn the_cap_parses_from_1_and_2_mib_callers() {
    // XML_MAX_NESTING levels are within the scan; the import parses them on its own parse thread
    // and answers typed (the walk's max_depth 32) from either caller stack.
    let cap = hsk_studio_package::XML_MAX_NESTING;
    let text = format!("{SVG_OPEN}{}{}</svg>", "<g>".repeat(cap - 1), "</g>".repeat(cap - 1));
    assert!(hsk_studio_package::xml_nesting_within(&text, cap));
    for stack in CALLERS {
        assert_eq!(import_from(stack, text.clone(), ImportPolicy::default()), Err(VectorError::BudgetExceeded("depth")));
    }
}

/// Measurement probe (item 1: the smallest element depth that aborts roxmltree, with the options
/// `import_svg` uses, on a 1 MiB and a 2 MiB thread). Not a gate: run by hand with
/// `--run-ignored only -E 'test(svg_depth_abort_probe)'`; it bisects by re-running this binary's
/// `svg_depth_probe_child` (one parse per child process, so an abort ends only the child) and prints
/// `PROBE roxmltree stack=<bytes> aborts_at=<depth>`.
#[test]
#[ignore = "measurement probe, run by hand"]
fn svg_depth_abort_probe() {
    let exe = std::env::current_exe().expect("test binary");
    let survives = |stack: usize, depth: usize| {
        std::process::Command::new(&exe)
            .args(["--exact", "svg_depth_probe_child", "--ignored", "--nocapture"])
            .env("HOSTILE_PROBE_STACK", stack.to_string())
            .env("HOSTILE_PROBE_DEPTH", depth.to_string())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    };
    for stack in [1usize << 20, 2 << 20] {
        let (mut lo, mut hi) = (1usize, 1usize << 16);
        if survives(stack, hi) {
            println!("PROBE roxmltree stack={stack} aborts_at=>{hi}");
            continue;
        }
        while hi - lo > 1 {
            let mid = lo + (hi - lo) / 2;
            if survives(stack, mid) { lo = mid } else { hi = mid }
        }
        println!("PROBE roxmltree stack={stack} aborts_at={hi} bytes_per_level~{}", stack / hi);
    }
}

#[test]
#[ignore = "child of svg_depth_abort_probe"]
fn svg_depth_probe_child() {
    let var = |k: &str| std::env::var(k).ok().and_then(|v| v.parse::<usize>().ok());
    let (Some(stack), Some(depth)) = (var("HOSTILE_PROBE_STACK"), var("HOSTILE_PROBE_DEPTH")) else {
        println!("NOT_PROVEN: probe child run without HOSTILE_PROBE_STACK / HOSTILE_PROBE_DEPTH");
        return;
    };
    let text = format!("{SVG_OPEN}{}{}</svg>", "<g>".repeat(depth.saturating_sub(1)), "</g>".repeat(depth.saturating_sub(1)));
    let n = on_stack(stack, move || {
        let options = roxmltree::ParsingOptions { allow_dtd: false, nodes_limit: u32::MAX, entity_resolver: None };
        roxmltree::Document::parse_with_options(&text, options).map(|d| d.descendants().count()).unwrap_or(0)
    });
    println!("PROBE child stack={stack} depth={depth} nodes={n}");
}
