//! Lane SMB-LAYOUT-COMPOSER-DEPTH-1, output identity of the Adobe paragraph composer: a differential
//! table over generated paragraphs against bytes captured from the base sha. `composer_depth_table.txt`
//! was written by `capture_the_table` on sha 091c4d1b, the recursive `Para::best`, and is never
//! re-blessed: a row that differs means the composer's output changed. Compared per case: every
//! composer-decided value of every line, floats by their bits, or the refusal code.
//!
//! The generator is deterministic: xorshift64* from a fixed seed, a table of per-glyph advances.
//! Both windows (justified 7 lines, ragged the whole paragraph), forced breaks (U+2028 outside
//! shaping, runs of them, leading and trailing), hyphen runs with ladder limits, letter and glyph
//! scaling ranges, and threads of two frames of different measures so the width changes by line.
mod support_layout;
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::*;
use hsk_studio_layout::*;
use std::fmt::Write as _;
use support_layout::*;

const LS: char = '\u{2028}';
const TABLE: &str = include_str!("composer_depth_table.txt");
const SHORT_CASES: usize = 1500;
const MEDIUM_RAGGED: usize = 40;
const MEDIUM_JUSTIFIED: usize = 20;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x9E37_79B9_7F4A_7C15)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() >> 33) as usize % n
    }
    fn pick<T: Copy>(&mut self, of: &[T]) -> T {
        of[self.below(of.len())]
    }
}

const SHORT_WORDS: &[&str] = &["a", "of", "to", "the", "word", "line", "ink", "set", "glyph", "frame", "story", "type"];
const LONG_WORDS: &[&str] =
    &["typographic", "internationalization", "composition", "hyphenation", "differences", "justification", "paragraph", "everything", "consecutive", "information"];

#[derive(Debug)]
struct Case {
    text: String,
    alignment: Alignment,
    justify: JustifyParams,
    hyphen_limit: Option<u8>,
    /// Advance per glyph: a table walk, so line widths are not multiples of one value.
    advances: [f64; 4],
    /// Measure of the first frame and, with a second frame, its height in lines and that measure.
    measure: f64,
    second: Option<(usize, f64)>,
}

fn text(rng: &mut Rng, words: usize, long_pct: usize, forced_pct: usize) -> String {
    let mut out = String::new();
    if forced_pct > 0 && rng.below(6) == 0 {
        out.extend(std::iter::repeat_n(LS, 1 + rng.below(3)));
    }
    for w in 0..words {
        let word = if rng.below(100) < long_pct { rng.pick(LONG_WORDS) } else { rng.pick(SHORT_WORDS) };
        out.push_str(word);
        if w + 1 < words {
            if rng.below(100) < forced_pct {
                out.extend(std::iter::repeat_n(LS, 1 + rng.below(3) * rng.below(2)));
            } else {
                out.push(' ');
            }
        }
    }
    if forced_pct > 0 && rng.below(6) == 0 {
        out.push(LS);
    }
    out
}

fn case(rng: &mut Rng, words: usize, alignment: Option<Alignment>) -> Case {
    let long_pct = rng.pick(&[0usize, 20, 60, 100]);
    let forced_pct = rng.pick(&[0usize, 0, 10, 30]);
    let spacing = |min, desired, max| Spacing { min, desired, max };
    let base = JustifyParams::default();
    let justify = match rng.below(6) {
        0 => JustifyParams { word_spacing: spacing(0.9, 1.0, 1.1), ..base },
        1 => JustifyParams { word_spacing: spacing(0.5, 1.0, 2.0), ..base },
        2 => JustifyParams { letter_spacing: spacing(-0.05, 0.0, 0.05), reference_space_pt: 3.0, ..base },
        3 => JustifyParams { glyph_scaling: spacing(0.95, 1.0, 1.05), ..base },
        _ => base,
    };
    Case {
        text: text(rng, words, long_pct, forced_pct),
        alignment: alignment.unwrap_or_else(|| rng.pick(&[Alignment::Left, Alignment::Left, Alignment::Right, Alignment::Justified, Alignment::Justified, Alignment::JustifiedAll])),
        justify,
        hyphen_limit: rng.pick(&[None, None, Some(0u8), Some(1), Some(2), Some(3)]),
        advances: *rng.pick(&[&[6.0, 6.0, 6.0, 6.0], &[4.7, 5.3, 6.1, 3.9], &[2.25, 7.5, 5.0, 5.5]]),
        measure: rng.pick(&[36.0, 60.0, 72.0, 90.0, 120.0, 150.0, 180.0, 240.0, 330.0]),
        second: (rng.below(4) == 0).then(|| (1 + rng.below(4), rng.pick(&[48.0, 84.0, 132.0, 210.0]))),
    }
}

fn cases() -> Vec<Case> {
    let mut rng = Rng(0x5354_5544_494F_0056);
    let mut out = Vec::with_capacity(SHORT_CASES + MEDIUM_RAGGED + MEDIUM_JUSTIFIED);
    for _ in 0..SHORT_CASES {
        let words = 1 + rng.below(40);
        out.push(case(&mut rng, words, None));
    }
    for _ in 0..MEDIUM_RAGGED {
        let words = 40 + rng.below(40);
        out.push(case(&mut rng, words, Some(Alignment::Left)));
    }
    for _ in 0..MEDIUM_JUSTIFIED {
        let words = 150 + rng.below(150);
        out.push(case(&mut rng, words, Some(Alignment::Justified)));
    }
    out
}

fn compose(c: &Case) -> Result<LayoutProposal, LayoutError> {
    let tall = 2_000_000.0;
    let layers = match c.second {
        None => vec![frame_layer(1, 1, spec(0, 10.0, 10.0, c.measure, tall), None, None)],
        Some((lines, w2)) => vec![
            frame_layer(1, 1, spec(0, 10.0, 10.0, c.measure, 12.0 * lines as f64), None, Some(2)),
            frame_layer(2, 1, spec(1, 10.0, 10.0, w2, tall), Some(1), None),
        ],
    };
    let doc = document(layers, [PageSide::LeftHand, PageSide::RightHand]);
    let (st, _) = story(1, &c.text, &[(0, c.text.len() as u64)]);
    let source = SourceSpan { start: 0, end: c.text.len() as u64 };
    let mut p = ShapedParagraph::from_fixed_advance(&c.text, source, 6.0, 12.0, 10.0).unwrap();
    p.glyphs.retain(|g| !c.text[g.cluster.start as usize..].starts_with(LS));
    for (i, g) in p.glyphs.iter_mut().enumerate() {
        g.advance_pt = c.advances[i % c.advances.len()];
    }
    let paragraphs = vec![p];
    let hyphenation = c.hyphen_limit.map(|consecutive_limit| HyphenationParams { hyphen_advance_pt: 4.0, min_word_len: 5, min_before: 2, min_after: 2, consecutive_limit, ..HyphenationParams::default() });
    reflow_typeset(
        &ReflowRequest {
            document: &doc,
            story: &st,
            resolved_text: None,
            paragraphs: &paragraphs,
            obstacles: &[],
            composer: Composer::AdobeParagraph(AdobeParams::default()),
            space: SpaceParams::default(),
            expected_revision: doc.revision,
            limits: Limits::default(),
        },
        &TypesetOptions { alignment: c.alignment, justify: Some(c.justify), hyphenation, ..Default::default() },
        &CancellationToken::default(),
    )
}

fn bits(v: f64) -> u64 {
    v.to_bits()
}
/// Every composer-decided value of the case, in a fixed form.
fn answer_bytes(answer: &Result<LayoutProposal, LayoutError>) -> String {
    let p = match answer {
        Ok(p) => p,
        Err(e) => return format!("refused {}", e.code()),
    };
    let mut s = String::new();
    for (f, frame) in p.frames.iter().enumerate() {
        for l in &frame.lines {
            let j = l.justification.map(|j| (bits(j.word_scale), bits(j.glyph_scale), bits(j.letter_gap_pt), bits(j.residual_pt)));
            writeln!(
                s,
                "{f} {:?} {:x} {:x} {:x} {:x} {:x} {}..{} {:?} {} {:?} {} {:?} {}",
                l.glyph_range,
                bits(l.baseline_y_pt),
                bits(l.x_start_pt),
                bits(l.width_pt),
                bits(l.available_pt),
                bits(l.hang_pt),
                l.source.start,
                l.source.end,
                l.ratio.map(bits),
                l.overfull,
                j,
                l.hyphenated,
                l.overfull,
                l.column,
            )
            .unwrap();
        }
    }
    writeln!(s, "overset {:?}", p.overset.as_ref().map(|o| (o.start, o.end))).unwrap();
    s
}
fn fnv1a64(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, b| (h ^ u64::from(*b)).wrapping_mul(0x0000_0100_0000_01b3))
}
fn row(i: usize, c: &Case) -> String {
    let Ok(answer) = std::panic::catch_unwind(|| compose(c)) else {
        return format!("{i} panic");
    };
    let lines = answer.as_ref().map_or(0, |p| p.frames.iter().map(|f| f.lines.len()).sum::<usize>());
    format!("{i} {lines} {:016x}", fnv1a64(answer_bytes(&answer).as_bytes()))
}

#[derive(Default, Debug)]
struct Coverage {
    ragged: usize,
    justified: usize,
    forced: usize,
    hyphen_runs: usize,
    overfull: usize,
    threaded: usize,
    refused: usize,
    max_lines: usize,
}
fn coverage(all: &[Case]) -> Coverage {
    let mut n = Coverage::default();
    for c in all {
        if matches!(c.alignment, Alignment::Justified | Alignment::JustifiedAll) {
            n.justified += 1;
        } else {
            n.ragged += 1;
        }
        n.forced += usize::from(c.text.contains(LS));
        n.threaded += usize::from(c.second.is_some());
        match compose(c) {
            Ok(p) => {
                let lines: Vec<&Line> = p.frames.iter().flat_map(|f| f.lines.iter()).collect();
                n.max_lines = n.max_lines.max(lines.len());
                n.overfull += usize::from(lines.iter().any(|l| l.overfull));
                n.hyphen_runs += usize::from(lines.windows(2).any(|w| w[0].hyphenated && w[1].hyphenated));
            }
            Err(_) => n.refused += 1,
        }
    }
    n
}

/// Writes the table: run on the base sha only, with `STUDIO_COMPOSER_TABLE_OUT` naming the file.
#[test]
#[ignore = "writes the table from the checked-out composer; run on the base sha only, never to re-bless"]
fn capture_the_table() {
    let out = std::env::var_os("STUDIO_COMPOSER_TABLE_OUT").expect("STUDIO_COMPOSER_TABLE_OUT names the output file");
    let mut table = String::new();
    for (i, c) in cases().iter().enumerate() {
        let t = std::time::Instant::now();
        let r = row(i, c);
        if r.ends_with("panic") {
            println!("PANIC_CASE {i} {c:?}");
        }
        if t.elapsed().as_secs() >= 5 {
            println!("SLOW_CASE {i} {:?} words={} measure={}", t.elapsed(), c.text.split(' ').count(), c.measure);
        }
        table.push_str(&r);
        table.push('\n');
    }
    std::fs::write(out, table).unwrap();
}

/// Every row of the table, byte for byte against the base sha.
#[test]
fn every_generated_paragraph_composes_as_on_the_base_sha() {
    let all = cases();
    let want: Vec<&str> = TABLE.lines().collect();
    assert_eq!(want.len(), all.len());
    let differ: Vec<String> = all
        .iter()
        .enumerate()
        .filter_map(|(i, c)| {
            let got = row(i, c);
            (got != want[i]).then(|| format!("{got} want {} {c:?}", want[i]))
        })
        .take(5)
        .collect();
    assert!(differ.is_empty(), "{differ:#?}");
}

/// The table holds what it claims: both windows, forced breaks, hyphen runs, overfull lines, widths
/// that change by line, and long paragraphs of both alignments.
#[test]
fn the_generated_table_covers_its_claims() {
    let n = coverage(&cases());
    assert!(n.ragged >= 500 && n.justified >= 500, "{n:?}");
    assert!(n.forced >= 300, "{n:?}");
    assert!(n.hyphen_runs >= 100, "{n:?}");
    assert!(n.overfull >= 50, "{n:?}");
    assert!(n.threaded >= 300, "{n:?}");
    assert!(n.max_lines >= 50, "{n:?}");
    assert_eq!(n.refused, 0, "{n:?}");
}
