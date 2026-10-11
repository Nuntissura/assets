//! The two line breakers over the box/glue/penalty item model: a greedy first-fit single-line
//! composer and the Knuth-Plass optimum-fit paragraph composer (badness = 100 * |r|^3, squared
//! demerits, four fitness classes, three passes). Both are pure and deterministic: no hashing, ties
//! resolve to the earlier candidate.
use crate::LayoutError;
use crate::paragraph::{FORCED, Item, Kind};
use hsk_studio_accord::CancellationToken;
use std::collections::BTreeMap;

const EPS: f64 = 1e-9;
const NONE: usize = usize::MAX;

/// Knuth-Plass tuning. Defaults follow TeX: `\linepenalty` 10, `\adjdemerits` 10000, `\tolerance`
/// 200 for the first pass and unlimited (badness is capped at 10000) for the second.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct KnuthPlassParams {
    pub line_penalty: f64,
    pub adjacent_fitness_demerits: f64,
    pub first_pass_tolerance: f64,
    pub second_pass_tolerance: f64,
}
impl Default for KnuthPlassParams {
    fn default() -> Self {
        Self {
            line_penalty: 10.0,
            adjacent_fitness_demerits: 10_000.0,
            first_pass_tolerance: 200.0,
            second_pass_tolerance: 10_000.0,
        }
    }
}

/// Which line breaker composes a paragraph. `FirstFit` is the greedy single-line composer (each
/// line takes as much as fits); `KnuthPlass` minimises total demerits over the whole paragraph.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Composer {
    FirstFit,
    KnuthPlass(KnuthPlassParams),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct LineBreak {
    pub item: usize,
    pub overfull: bool,
    pub ratio: Option<f64>,
}

pub(crate) struct Sums {
    w: Vec<f64>,
    y: Vec<f64>,
    z: Vec<f64>,
    inf: Vec<u32>,
}
impl Sums {
    pub fn new(items: &[Item]) -> Self {
        let mut s = Self {
            w: vec![0.0],
            y: vec![0.0],
            z: vec![0.0],
            inf: vec![0],
        };
        for it in items {
            let glue_or_box = match it.kind {
                Kind::Box => true,
                Kind::Glue => !it.trailing,
                Kind::Penalty => false,
            };
            let (w, y, z) = if glue_or_box {
                (it.w, it.y, it.z)
            } else {
                (0.0, 0.0, 0.0)
            };
            s.w.push(s.w[s.w.len() - 1] + w);
            s.y.push(s.y[s.y.len() - 1] + y);
            s.z.push(s.z[s.z.len() - 1] + z);
            s.inf.push(s.inf[s.inf.len() - 1] + u32::from(it.inf_stretch && !it.trailing));
        }
        s
    }
    /// Natural width, stretch, shrink and infinite-stretch flag of the line `items[s..i)` that ends
    /// with a break at item `i` (a penalty contributes its own width; a glue break is discarded).
    fn measure(&self, items: &[Item], s: usize, i: usize) -> (f64, f64, f64, bool) {
        let pen = if items[i].kind == Kind::Penalty {
            items[i].w
        } else {
            0.0
        };
        (
            self.w[i] - self.w[s] + pen,
            self.y[i] - self.y[s],
            self.z[i] - self.z[s],
            self.inf[i] > self.inf[s],
        )
    }
}

fn forced(it: &Item) -> bool {
    it.kind == Kind::Penalty && it.p == FORCED
}
fn legal(items: &[Item], i: usize) -> bool {
    let it = &items[i];
    match it.kind {
        Kind::Box => false,
        Kind::Glue => it.breakable && i > 0 && items[i - 1].kind == Kind::Box,
        Kind::Penalty => it.p < f64::INFINITY,
    }
}
/// First item of the next line: after a forced break the very next item; otherwise the next box
/// (glue and penalties right after a break are discarded).
pub(crate) fn line_start(items: &[Item], b: usize) -> usize {
    if forced(&items[b]) {
        return b + 1;
    }
    (b + 1..items.len())
        .find(|j| items[*j].kind == Kind::Box)
        .unwrap_or(items.len())
}

fn check(cancel: &CancellationToken, work: &mut u32) -> Result<(), LayoutError> {
    *work += 1;
    if (*work).is_multiple_of(256) {
        cancel.check().map_err(|_| LayoutError::Canceled)?;
    }
    Ok(())
}

pub(crate) fn first_fit(
    items: &[Item],
    sums: &Sums,
    width: &dyn Fn(usize) -> f64,
    cancel: &CancellationToken,
) -> Result<Vec<LineBreak>, LayoutError> {
    let (mut breaks, mut start, mut best, mut work) = (Vec::new(), 0usize, None::<usize>, 0u32);
    let mut i = 0;
    while i < items.len() {
        check(cancel, &mut work)?;
        if i <= start || !legal(items, i) {
            i += 1;
            continue;
        }
        let (w, ..) = sums.measure(items, start, i);
        if w <= width(breaks.len()) + EPS {
            if forced(&items[i]) {
                breaks.push(LineBreak {
                    item: i,
                    overfull: false,
                    ratio: None,
                });
                start = line_start(items, i);
                best = None;
            } else {
                best = Some(i);
            }
        } else if let Some(b) = best.take() {
            breaks.push(LineBreak {
                item: b,
                overfull: false,
                ratio: None,
            });
            start = line_start(items, b);
            i = b;
        } else {
            breaks.push(LineBreak {
                item: i,
                overfull: true,
                ratio: None,
            });
            start = line_start(items, i);
        }
        i += 1;
    }
    Ok(breaks)
}

fn badness(r: f64) -> f64 {
    (100.0 * r.abs().powi(3)).min(10_000.0)
}
fn fitness(r: f64) -> u8 {
    if r < -0.5 {
        0
    } else if r <= 0.5 {
        1
    } else if r <= 1.0 {
        2
    } else {
        3
    }
}

struct Node {
    pos: usize,
    line: usize,
    class: u8,
    demerits: f64,
    prev: usize,
    ratio: f64,
    overfull: bool,
    has_successor: bool,
}
struct Candidate {
    total: f64,
    from: usize,
    ratio: f64,
    overfull: bool,
}

fn knuth_plass_pass(
    items: &[Item],
    sums: &Sums,
    width: &dyn Fn(usize) -> f64,
    params: &KnuthPlassParams,
    tolerance: f64,
    allow_overfull: bool,
    cancel: &CancellationToken,
) -> Result<Option<Vec<LineBreak>>, LayoutError> {
    let mut arena = vec![Node {
        pos: NONE,
        line: 0,
        class: 1,
        demerits: 0.0,
        prev: NONE,
        ratio: 0.0,
        overfull: false,
        has_successor: false,
    }];
    let mut active = vec![0usize];
    let mut work = 0u32;
    for i in 0..items.len() {
        if !legal(items, i) {
            continue;
        }
        check(cancel, &mut work)?;
        let is_forced = forced(&items[i]);
        let penalty = if items[i].kind == Kind::Penalty {
            items[i].p
        } else {
            0.0
        };
        let mut candidates: BTreeMap<(usize, u8), Candidate> = BTreeMap::new();
        let mut retained = Vec::with_capacity(active.len());
        for &ai in &active {
            let s = if arena[ai].pos == NONE {
                0
            } else {
                line_start(items, arena[ai].pos)
            };
            if i <= s {
                retained.push(ai);
                continue;
            }
            let line = arena[ai].line;
            let l = width(line);
            let (w, y, z, inf) = sums.measure(items, s, i);
            let r = if w < l - EPS {
                if inf {
                    0.0
                } else if y > 0.0 {
                    (l - w) / y
                } else {
                    f64::INFINITY
                }
            } else if w > l + EPS {
                if z > 0.0 {
                    (l - w) / z
                } else {
                    f64::NEG_INFINITY
                }
            } else {
                0.0
            };
            let overfull = r < -1.0 - EPS;
            let take = if overfull {
                allow_overfull && !arena[ai].has_successor
            } else {
                badness(r) <= tolerance
            };
            if take {
                arena[ai].has_successor = true;
                let b = if overfull { 10_000.0 } else { badness(r) };
                let class = if overfull { 0 } else { fitness(r) };
                let base = params.line_penalty + b;
                let mut d = if penalty >= 0.0 {
                    (base + penalty).powi(2)
                } else if penalty > FORCED {
                    base.powi(2) - penalty.powi(2)
                } else {
                    base.powi(2)
                };
                if class.abs_diff(arena[ai].class) > 1 {
                    d += params.adjacent_fitness_demerits;
                }
                let total = arena[ai].demerits + d;
                let key = (line + 1, class);
                if candidates.get(&key).is_none_or(|c| total < c.total) {
                    candidates.insert(
                        key,
                        Candidate {
                            total,
                            from: ai,
                            ratio: r,
                            overfull,
                        },
                    );
                }
            }
            if !(overfull || is_forced) {
                retained.push(ai);
            }
        }
        active = retained;
        let mut minimum: BTreeMap<usize, f64> = BTreeMap::new();
        for ((line, _), c) in &candidates {
            minimum
                .entry(*line)
                .and_modify(|m| *m = m.min(c.total))
                .or_insert(c.total);
        }
        for ((line, class), c) in candidates {
            if c.total <= minimum[&line] + params.adjacent_fitness_demerits {
                arena.push(Node {
                    pos: i,
                    line,
                    class,
                    demerits: c.total,
                    prev: c.from,
                    ratio: c.ratio,
                    overfull: c.overfull,
                    has_successor: false,
                });
                active.push(arena.len() - 1);
            }
        }
        if active.is_empty() {
            return Ok(None);
        }
    }
    let last = items.len() - 1;
    let mut best: Option<usize> = None;
    for &ai in &active {
        if arena[ai].pos == last && best.is_none_or(|b| arena[ai].demerits < arena[b].demerits) {
            best = Some(ai);
        }
    }
    let Some(mut at) = best else {
        return Ok(None);
    };
    let mut breaks = Vec::new();
    while arena[at].pos != NONE {
        let n = &arena[at];
        breaks.push(LineBreak {
            item: n.pos,
            overfull: n.overfull,
            ratio: Some(n.ratio),
        });
        at = n.prev;
    }
    breaks.reverse();
    Ok(Some(breaks))
}

pub(crate) fn knuth_plass(
    items: &[Item],
    sums: &Sums,
    width: &dyn Fn(usize) -> f64,
    params: &KnuthPlassParams,
    cancel: &CancellationToken,
) -> Result<Vec<LineBreak>, LayoutError> {
    for (tolerance, overfull) in [
        (params.first_pass_tolerance, false),
        (params.second_pass_tolerance, false),
        (params.second_pass_tolerance, true),
    ] {
        if let Some(b) = knuth_plass_pass(items, sums, width, params, tolerance, overfull, cancel)? {
            return Ok(b);
        }
    }
    first_fit(items, sums, width, cancel)
}
