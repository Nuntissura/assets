//! Dinic maximum flow / minimum s-t cut over integer capacities (clean-room from the published
//! algorithm: BFS level graph, blocking flow by depth-first augmentation with current-arc
//! pointers). Used by the graph-cut selection tools ([`super::quick`]). Arcs are stored in
//! forward-star lists; arc `a ^ 1` is the reverse of arc `a`. The depth-first search is
//! iterative (grid paths can be as long as the node count). Every arc scan counts against a
//! caller step budget so a hostile graph refuses `BudgetExceeded` instead of running unbounded.
#![deny(clippy::arithmetic_side_effects)]
use crate::Error;

const NONE: u32 = u32::MAX;

/// Bytes one arc costs (`to` u32 + `next` u32 + `cap` i64).
pub const BYTES_PER_ARC: u64 = 16;
/// Bytes one node costs (`first`, `level`, `it`: 3 x 4).
pub const BYTES_PER_NODE: u64 = 12;

/// A flow network. Build with [`FlowGraph::with_capacity`] + [`FlowGraph::add_edge`].
#[derive(Clone, Debug)]
pub struct FlowGraph {
    first: Vec<u32>,
    to: Vec<u32>,
    next: Vec<u32>,
    cap: Vec<i64>,
}

impl FlowGraph {
    /// `nodes` nodes and room for `edges` edges (2 arcs each). The caller admits the bytes
    /// ([`BYTES_PER_NODE`], [`BYTES_PER_ARC`]) before calling.
    pub fn with_capacity(nodes: usize, edges: usize) -> Result<Self, Error> {
        if u32::try_from(nodes).is_err() {
            return Err(Error::Overflow);
        }
        let arcs = edges.checked_mul(2).ok_or(Error::Overflow)?;
        if u32::try_from(arcs).map_or(true, |a| a == NONE) {
            return Err(Error::Overflow);
        }
        Ok(Self {
            first: vec![NONE; nodes],
            to: Vec::with_capacity(arcs),
            next: Vec::with_capacity(arcs),
            cap: Vec::with_capacity(arcs),
        })
    }

    pub fn nodes(&self) -> usize {
        self.first.len()
    }

    fn push_arc(&mut self, u: usize, v: usize, c: i64) -> Result<(), Error> {
        let a = u32::try_from(self.to.len()).map_err(|_| Error::Overflow)?;
        if a == NONE {
            return Err(Error::Overflow);
        }
        let head = self.first.get_mut(u).ok_or(Error::InvalidInput)?;
        self.next.push(*head);
        *head = a;
        self.to.push(u32::try_from(v).map_err(|_| Error::Overflow)?);
        self.cap.push(c);
        Ok(())
    }

    /// Edge `u -> v` with capacity `cap_uv` and reverse capacity `cap_vu` (both `>= 0`).
    pub fn add_edge(&mut self, u: usize, v: usize, cap_uv: i64, cap_vu: i64) -> Result<(), Error> {
        if cap_uv < 0 || cap_vu < 0 || u >= self.nodes() || v >= self.nodes() {
            return Err(Error::InvalidInput);
        }
        self.push_arc(u, v, cap_uv)?;
        self.push_arc(v, u, cap_vu)
    }

    fn bfs(&self, s: usize, level: &mut [i32], queue: &mut Vec<u32>, steps: &mut u64, max_steps: u64) -> Result<(), Error> {
        level.fill(-1);
        queue.clear();
        if let Some(l) = level.get_mut(s) {
            *l = 0;
        }
        queue.push(u32::try_from(s).map_err(|_| Error::Overflow)?);
        let mut head = 0usize;
        while let Some(&u) = queue.get(head) {
            head = head.saturating_add(1);
            let u = u as usize;
            let lu = level.get(u).copied().unwrap_or(-1);
            let mut a = self.first.get(u).copied().unwrap_or(NONE);
            while a != NONE {
                *steps = steps.saturating_add(1);
                if *steps > max_steps {
                    return Err(Error::BudgetExceeded);
                }
                let ai = a as usize;
                let v = self.to.get(ai).copied().unwrap_or(NONE) as usize;
                if self.cap.get(ai).copied().unwrap_or(0) > 0
                    && let Some(lv) = level.get_mut(v)
                    && *lv < 0
                {
                    *lv = lu.saturating_add(1);
                    queue.push(u32::try_from(v).map_err(|_| Error::Overflow)?);
                }
                a = self.next.get(ai).copied().unwrap_or(NONE);
            }
        }
        Ok(())
    }

    /// One augmenting path in the level graph (0 when none is left).
    #[allow(clippy::too_many_arguments)]
    fn augment(
        &mut self,
        s: usize,
        t: usize,
        level: &mut [i32],
        it: &mut [u32],
        path: &mut Vec<u32>,
        steps: &mut u64,
        max_steps: u64,
    ) -> Result<i64, Error> {
        path.clear();
        let mut u = s;
        loop {
            if u == t {
                let f = path
                    .iter()
                    .map(|a| self.cap.get(*a as usize).copied().unwrap_or(0))
                    .min()
                    .unwrap_or(0);
                for a in path.iter() {
                    let ai = *a as usize;
                    if let Some(c) = self.cap.get_mut(ai) {
                        *c = c.saturating_sub(f);
                    }
                    if let Some(c) = self.cap.get_mut(ai ^ 1) {
                        *c = c.saturating_add(f);
                    }
                }
                return Ok(f);
            }
            let lu = level.get(u).copied().unwrap_or(-1);
            let mut advanced = false;
            loop {
                let a = it.get(u).copied().unwrap_or(NONE);
                if a == NONE {
                    break;
                }
                *steps = steps.saturating_add(1);
                if *steps > max_steps {
                    return Err(Error::BudgetExceeded);
                }
                let ai = a as usize;
                let v = self.to.get(ai).copied().unwrap_or(NONE) as usize;
                if self.cap.get(ai).copied().unwrap_or(0) > 0
                    && level.get(v).copied().unwrap_or(-1) == lu.saturating_add(1)
                {
                    path.push(a);
                    u = v;
                    advanced = true;
                    break;
                }
                if let Some(slot) = it.get_mut(u) {
                    *slot = self.next.get(ai).copied().unwrap_or(NONE);
                }
            }
            if !advanced {
                if u == s {
                    return Ok(0);
                }
                // Dead end: prune it and retreat past the arc that led here.
                if let Some(l) = level.get_mut(u) {
                    *l = -1;
                }
                let Some(a) = path.pop() else {
                    return Ok(0);
                };
                u = self.to.get((a as usize) ^ 1).copied().unwrap_or(NONE) as usize;
                if let Some(slot) = it.get_mut(u) {
                    *slot = self.next.get(a as usize).copied().unwrap_or(NONE);
                }
            }
        }
    }

    /// Maximum flow from `s` to `t`; at most `max_steps` arc scans, else `BudgetExceeded`.
    pub fn max_flow(&mut self, s: usize, t: usize, max_steps: u64) -> Result<i64, Error> {
        let n = self.nodes();
        if s >= n || t >= n || s == t {
            return Err(Error::InvalidInput);
        }
        let mut level = vec![-1i32; n];
        let mut queue = Vec::with_capacity(n);
        let mut path = Vec::new();
        let mut steps = 0u64;
        let mut flow = 0i64;
        loop {
            self.bfs(s, &mut level, &mut queue, &mut steps, max_steps)?;
            if level.get(t).copied().unwrap_or(-1) < 0 {
                return Ok(flow);
            }
            let mut it = self.first.clone();
            loop {
                let f = self.augment(s, t, &mut level, &mut it, &mut path, &mut steps, max_steps)?;
                if f == 0 {
                    break;
                }
                flow = flow.checked_add(f).ok_or(Error::Overflow)?;
            }
        }
    }

    /// Nodes reachable from `s` in the residual graph (the source side of a minimum cut after
    /// [`FlowGraph::max_flow`]).
    pub fn source_side(&self, s: usize) -> Vec<bool> {
        let mut seen = vec![false; self.nodes()];
        let mut stack = Vec::new();
        if let Some(v) = seen.get_mut(s) {
            *v = true;
            stack.push(s);
        }
        while let Some(u) = stack.pop() {
            let mut a = self.first.get(u).copied().unwrap_or(NONE);
            while a != NONE {
                let ai = a as usize;
                let v = self.to.get(ai).copied().unwrap_or(NONE) as usize;
                if self.cap.get(ai).copied().unwrap_or(0) > 0
                    && let Some(sv) = seen.get_mut(v)
                    && !*sv
                {
                    *sv = true;
                    stack.push(v);
                }
                a = self.next.get(ai).copied().unwrap_or(NONE);
            }
        }
        seen
    }
}
