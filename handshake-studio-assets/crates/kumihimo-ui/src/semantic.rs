//! The semantic tree: what `gui.tree` returns to a model and what the host's AccessKit adapter is fed from. Node shape
//! of STU-MDL-105 (id, author_id, node_id, role, label, value, disabled, actions, bounds, children) plus the numeric
//! fields it requires. Only nodes with an author_id appear; decorative nodes fold into their nearest identified
//! ancestor. Truncation at a node cap is explicit (STU-MDL-108).
use crate::geom::Rect;
use crate::tree::{Numeric, Role, Tree};

#[derive(Clone, Debug, PartialEq)]
pub struct SemanticNode {
    /// `stable_id` of STU-MDL-103 item 2: "hs-" + author_id with dots as hyphens.
    pub id: String,
    pub author_id: String,
    /// Hashed node id derived from the author_id (STU-MDL-104): FNV-1a 64 here. UNVERIFIED that the shell uses the
    /// same hash; the host adapter may rehash, the derivation from author_id is what is fixed.
    pub node_id: u64,
    pub role: Role,
    pub label: Option<String>,
    pub value: Option<String>,
    pub numeric: Option<Numeric>,
    pub disabled: bool,
    pub selected: bool,
    pub focused: bool,
    pub actions: Vec<&'static str>,
    /// Logical px.
    pub bounds: Rect,
    pub children: Vec<SemanticNode>,
}

/// The semantic tree of one panel, with the explicit truncation flag.
#[derive(Clone, Debug, PartialEq)]
pub struct Semantic {
    pub roots: Vec<SemanticNode>,
    pub nodes: usize,
    pub truncated: bool,
}

pub fn fnv64(s: &str) -> u64 {
    let mut h: u64 = 0xCBF2_9CE4_8422_2325;
    for b in s.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01B3);
    }
    h
}

/// `stable_id` of an author_id (STU-MDL-103 item 2).
pub fn stable_id(author_id: &str) -> String {
    format!("hs-{}", author_id.replace('.', "-"))
}

/// True when `id` matches STU-MDL-100/103: dotted kebab-case segments of `[a-z0-9-]`, at least two, with an optional
/// `#<n>` instance suffix, n >= 2.
pub fn valid_author_id(id: &str) -> bool {
    let (path, inst) = match id.split_once('#') {
        Some((p, n)) => (p, Some(n)),
        None => (id, None),
    };
    if let Some(n) = inst {
        match n.parse::<u32>() {
            Ok(v) if v >= 2 && !n.starts_with('0') => {}
            _ => return false,
        }
    }
    let segs: Vec<&str> = path.split('.').collect();
    segs.len() >= 2
        && segs.iter().all(|s| {
            !s.is_empty() && !s.starts_with('-') && !s.ends_with('-') && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        })
}

/// Builds the semantic tree of `tree`, keeping at most `max_nodes` nodes.
pub fn semantic(tree: &Tree, max_nodes: usize) -> Semantic {
    let mut out = Semantic { roots: Vec::new(), nodes: 0, truncated: false };
    if tree.is_empty() {
        return out;
    }
    let mut roots = Vec::new();
    collect(tree, crate::tree::NodeId(0), &mut roots, &mut out, max_nodes, 0);
    out.roots = roots;
    out
}

fn collect(tree: &Tree, id: crate::tree::NodeId, into: &mut Vec<SemanticNode>, st: &mut Semantic, max: usize, depth: usize) {
    if depth >= crate::layout::MAX_DEPTH {
        st.truncated = true;
        return;
    }
    let n = tree.get(id);
    if n.author_id.is_empty() {
        for &c in &n.children {
            collect(tree, c, into, st, max, depth + 1);
        }
        return;
    }
    if st.nodes >= max {
        st.truncated = true;
        return;
    }
    st.nodes += 1;
    let mut node = SemanticNode {
        id: stable_id(&n.author_id),
        author_id: n.author_id.clone(),
        node_id: fnv64(&n.author_id),
        role: n.role,
        label: n.label.clone(),
        value: n.value.clone(),
        numeric: n.numeric,
        disabled: tree.effectively_disabled(id),
        selected: n.flags.selected || n.flags.toggled,
        focused: n.flags.focused,
        actions: n.actions.iter().map(|a| a.name()).collect(),
        bounds: n.bounds,
        children: Vec::new(),
    };
    for &c in &n.children {
        collect(tree, c, &mut node.children, st, max, depth + 1);
    }
    into.push(node);
}

fn esc(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}
fn num(v: f64) -> String {
    if v.is_finite() { format!("{v}") } else { "null".into() }
}
fn node_json(n: &SemanticNode, out: &mut String) {
    out.push_str("{\"id\":");
    esc(&n.id, out);
    out.push_str(",\"author_id\":");
    esc(&n.author_id, out);
    out.push_str(&format!(",\"node_id\":\"{:016x}\",\"role\":", n.node_id));
    esc(n.role.name(), out);
    out.push_str(",\"label\":");
    match &n.label {
        Some(l) => esc(l, out),
        None => out.push_str("null"),
    }
    out.push_str(",\"value\":");
    match &n.value {
        Some(v) => esc(v, out),
        None => out.push_str("null"),
    }
    if let Some(m) = n.numeric {
        out.push_str(&format!(",\"numeric\":{{\"value\":{},\"min\":{},\"max\":{},\"step\":{}}}", num(m.value), num(m.min), num(m.max), num(m.step)));
    }
    out.push_str(&format!(",\"disabled\":{},\"selected\":{},\"focused\":{},\"actions\":[", n.disabled, n.selected, n.focused));
    for (i, a) in n.actions.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        esc(a, out);
    }
    let b = n.bounds;
    out.push_str(&format!("],\"bounds\":[{},{},{},{}],\"children\":[", num(b.x0.into()), num(b.y0.into()), num(b.w().into()), num(b.h().into())));
    for (i, c) in n.children.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        node_json(c, out);
    }
    out.push_str("]}");
}

impl Semantic {
    /// `{"schema":"kumihimo.ui.tree@1","nodes":N,"truncated":B,"roots":[...]}`; bounds are logical [x, y, w, h].
    pub fn to_json(&self) -> String {
        let mut out = format!("{{\"schema\":\"kumihimo.ui.tree@1\",\"nodes\":{},\"truncated\":{},\"roots\":[", self.nodes, self.truncated);
        for (i, r) in self.roots.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            node_json(r, &mut out);
        }
        out.push_str("]}");
        out
    }
    /// Depth-first lookup by author_id.
    pub fn find(&self, author_id: &str) -> Option<&SemanticNode> {
        fn walk<'a>(ns: &'a [SemanticNode], id: &str) -> Option<&'a SemanticNode> {
            for n in ns {
                if n.author_id == id {
                    return Some(n);
                }
                if let Some(f) = walk(&n.children, id) {
                    return Some(f);
                }
            }
            None
        }
        walk(&self.roots, author_id)
    }
}
