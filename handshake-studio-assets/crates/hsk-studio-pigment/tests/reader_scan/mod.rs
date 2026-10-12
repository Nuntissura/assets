//! Source scan behind the plan-reader tripwire (PLR-073, PLR-080): which functions of the crate's
//! `src/` read the nodes of a plan or the entries of an envelope, and is each one classified?
//!
//! A read is the token pair `.nodes` or `.layers` after any receiver or parameter name
//! (`self.nodes`, `plan.nodes`, `render.nodes`, `extensions.layers`, `g.layers`), or the field
//! shorthand of a pattern or literal inside a function body (`RenderPlan { nodes, .. }`).
//! Comments and string and char literals are blanked first, so text cannot hide code or fake a
//! read. Every `fn` is found by the keyword (`pub(in ..)`, `const`, `unsafe`, `async` and
//! `extern` forms included) and its body by brace matching; a nested function owns its own reads.
//! Only a `#[cfg(test)]` item is skipped, never the rest of the file. The functions are pure over
//! `(path, text)` so the tests can feed mutated sources.
use std::path::Path;

/// One source file: its path relative to `src/` with `/` separators, and its text.
pub struct Source {
    pub path: String,
    pub text: String,
}

/// Every `.rs` file under `dir`, recursively, sorted by path.
pub fn read_sources(dir: &Path) -> Vec<Source> {
    fn walk(dir: &Path, root: &Path, out: &mut Vec<Source>) {
        let mut entries: Vec<_> = std::fs::read_dir(dir)
            .expect("source directory")
            .map(|entry| entry.expect("source entry").path())
            .collect();
        entries.sort();
        for path in entries {
            if path.is_dir() {
                walk(&path, root, out);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                let parts: Vec<String> = path
                    .strip_prefix(root)
                    .expect("inside the root")
                    .components()
                    .map(|part| part.as_os_str().to_string_lossy().into_owned())
                    .collect();
                out.push(Source {
                    path: parts.join("/"),
                    text: std::fs::read_to_string(&path).expect("source text"),
                });
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out
}

/// How a function that reads plan nodes or envelope entries is classified.
#[derive(Clone, Copy)]
pub enum Reads {
    /// Structure or a lookup: decided over every node or entry by design.
    Every,
    /// Capability: must go through the reachable set; every needle must occur in the body.
    Reachable(&'static [&'static str]),
}
/// One classified reader: the file (relative to `src/`), the function name and its class.
pub type ReaderRow = (&'static str, &'static str, Reads);

/// Where reads outside any function body are attributed.
pub const OUTSIDE_FN: &str = "<outside fn>";

/// A function with a body, and the lines of the reads that belong to it.
pub struct FnInfo {
    pub file: String,
    pub name: String,
    pub reads: Vec<usize>,
    /// Its body as code tokens, a space only between two words (see [`join_tokens`]).
    pub code: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Tok {
    Word(String),
    Sym(char),
}
struct Token {
    tok: Tok,
    line: usize,
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn blank(out: &mut [char], from: usize, to: usize) {
    let to = to.min(out.len());
    for ch in &mut out[from..to] {
        if *ch != '\n' {
            *ch = ' ';
        }
    }
}
/// A literal becomes one `"` followed by spaces (newlines kept).
fn literal(out: &mut [char], from: usize, to: usize) {
    blank(out, from, to);
    out[from] = '"';
}
fn string_end(c: &[char], from: usize) -> usize {
    let mut j = from;
    while j < c.len() {
        match c[j] {
            '\\' => j += 2,
            '"' => return j + 1,
            _ => j += 1,
        }
    }
    c.len()
}
fn raw_string_end(c: &[char], from: usize, hashes: usize) -> usize {
    let mut j = from;
    while j < c.len() {
        if c[j] == '"' && (1..=hashes).all(|k| c.get(j + k) == Some(&'#')) {
            return j + 1 + hashes;
        }
        j += 1;
    }
    c.len()
}

/// `text` as chars with comments blanked and string and char literals reduced to one `"`.
fn blank_non_code(text: &str) -> Vec<char> {
    let c: Vec<char> = text.chars().collect();
    let mut out = c.clone();
    let n = c.len();
    let mut i = 0;
    while i < n {
        let next = c.get(i + 1).copied();
        match c[i] {
            '/' if next == Some('/') => {
                let end = (i..n).find(|&j| c[j] == '\n').unwrap_or(n);
                blank(&mut out, i, end);
                i = end;
            }
            '/' if next == Some('*') => {
                let (mut depth, mut j) = (1usize, i + 2);
                while j < n && depth > 0 {
                    match (c[j], c.get(j + 1)) {
                        ('/', Some('*')) => {
                            depth += 1;
                            j += 2;
                        }
                        ('*', Some('/')) => {
                            depth -= 1;
                            j += 2;
                        }
                        _ => j += 1,
                    }
                }
                blank(&mut out, i, j);
                i = j;
            }
            '"' => {
                let end = string_end(&c, i + 1);
                literal(&mut out, i, end);
                i = end;
            }
            '\'' => {
                if next == Some('\\') {
                    // An escaped char literal: the closing quote follows the escaped character.
                    let end = (i + 3..n).find(|&j| c[j] == '\'').map_or(n, |j| j + 1);
                    literal(&mut out, i, end);
                    i = end;
                } else if c.get(i + 2) == Some(&'\'') {
                    literal(&mut out, i, i + 3);
                    i += 3;
                } else {
                    i += 1; // a lifetime or a label
                }
            }
            ch if is_word(ch) => {
                let end = (i..n).find(|&j| !is_word(c[j])).unwrap_or(n);
                let word: String = c[i..end].iter().collect();
                if matches!(word.as_str(), "r" | "br" | "cr") {
                    let hashes = (end..n).take_while(|&j| c[j] == '#').count();
                    if c.get(end + hashes) == Some(&'"') {
                        let close = raw_string_end(&c, end + hashes + 1, hashes);
                        literal(&mut out, i, close);
                        i = close;
                        continue;
                    }
                }
                i = end;
            }
            _ => i += 1,
        }
    }
    out
}

fn tokenize(code: &[char]) -> Vec<Token> {
    let mut tokens = Vec::new();
    let (mut line, mut i) = (1usize, 0usize);
    while i < code.len() {
        let ch = code[i];
        if ch == '\n' {
            line += 1;
            i += 1;
        } else if ch.is_whitespace() {
            i += 1;
        } else if is_word(ch) {
            let end = (i..code.len())
                .find(|&j| !is_word(code[j]))
                .unwrap_or(code.len());
            tokens.push(Token {
                tok: Tok::Word(code[i..end].iter().collect()),
                line,
            });
            i = end;
        } else {
            tokens.push(Token {
                tok: Tok::Sym(ch),
                line,
            });
            i += 1;
        }
    }
    tokens
}

/// The tokens as text, with a space only between two words: independent of line breaks and
/// indentation, so a needle matches however rustfmt lays the code out.
fn join_tokens(tokens: &[Token]) -> String {
    let mut out = String::new();
    let mut previous_word = false;
    for token in tokens {
        match &token.tok {
            Tok::Word(w) => {
                if previous_word {
                    out.push(' ');
                }
                out.push_str(w);
                previous_word = true;
            }
            Tok::Sym(c) => {
                out.push(*c);
                previous_word = false;
            }
        }
    }
    out
}
fn normalize(snippet: &str) -> String {
    let chars: Vec<char> = snippet.chars().collect();
    join_tokens(&tokenize(&chars))
}

/// The index of the `{` that opens the body of the `fn` whose name ends before `from`, or `None`
/// for a declaration without a body.
fn body_open(tokens: &[Token], from: usize) -> Option<usize> {
    let mut nest = 0usize;
    for (j, t) in tokens.iter().enumerate().skip(from) {
        match t.tok {
            Tok::Sym('(' | '[') => nest += 1,
            Tok::Sym(')' | ']') => nest = nest.saturating_sub(1),
            Tok::Sym('{') if nest == 0 => return Some(j),
            Tok::Sym(';') if nest == 0 => return None,
            _ => {}
        }
    }
    None
}
/// The index after the `}` that matches the `{` at `open`.
fn block_end(tokens: &[Token], open: usize) -> usize {
    let mut depth = 0usize;
    for (j, t) in tokens.iter().enumerate().skip(open) {
        match t.tok {
            Tok::Sym('{') => depth += 1,
            Tok::Sym('}') => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return j + 1;
                }
            }
            _ => {}
        }
    }
    tokens.len()
}
fn text_at(tokens: &[Token], i: usize) -> Option<String> {
    tokens.get(i).map(|t| match &t.tok {
        Tok::Word(w) => w.clone(),
        Tok::Sym(c) => c.to_string(),
    })
}

/// When `tokens[i..]` starts `#[cfg(test)]` followed by an item, the index after that item.
fn skip_cfg_test_item(tokens: &[Token], i: usize) -> Option<usize> {
    let attr = ["#", "[", "cfg", "(", "test", ")", "]"];
    if !attr
        .iter()
        .enumerate()
        .all(|(k, want)| text_at(tokens, i + k).as_deref() == Some(*want))
    {
        return None;
    }
    // Further attributes, then the item keyword; anything else (a variant, a field, an arm) is
    // scanned rather than skipped.
    let mut j = i + attr.len();
    while text_at(tokens, j).as_deref() == Some("#") {
        let mut nest = 0usize;
        loop {
            match tokens.get(j).map(|t| &t.tok) {
                Some(Tok::Sym('[')) => nest += 1,
                Some(Tok::Sym(']')) => {
                    nest = nest.saturating_sub(1);
                    if nest == 0 {
                        j += 1;
                        break;
                    }
                }
                None => return None,
                _ => {}
            }
            j += 1;
        }
    }
    const ITEMS: [&str; 15] = [
        "mod",
        "fn",
        "use",
        "const",
        "static",
        "impl",
        "struct",
        "enum",
        "type",
        "trait",
        "pub",
        "unsafe",
        "extern",
        "async",
        "macro_rules",
    ];
    let first = text_at(tokens, j)?;
    if !ITEMS.contains(&first.as_str()) {
        return None;
    }
    let mut nest = 0usize;
    while j < tokens.len() {
        match tokens[j].tok {
            Tok::Sym('(' | '[') => nest += 1,
            Tok::Sym(')' | ']') => nest = nest.saturating_sub(1),
            Tok::Sym(';') if nest == 0 => return Some(j + 1),
            Tok::Sym('{') if nest == 0 => return Some(block_end(tokens, j)),
            _ => {}
        }
        j += 1;
    }
    Some(tokens.len())
}

/// The line of a read of plan nodes or envelope entries that starts at `tokens[i]`.
fn read_at(tokens: &[Token], i: usize, in_braces: bool) -> Option<usize> {
    let field = |t: Option<&Token>| match t.map(|t| &t.tok) {
        Some(Tok::Word(w)) => w == "nodes" || w == "layers",
        _ => false,
    };
    // `.nodes` or `.layers`, not the tail of a `..` range.
    if tokens[i].tok == Tok::Sym('.')
        && i.checked_sub(1)
            .is_none_or(|p| tokens[p].tok != Tok::Sym('.'))
        && field(tokens.get(i + 1))
    {
        return Some(tokens[i + 1].line);
    }
    // The field shorthand or `nodes: pattern` of a struct pattern or literal in a function body.
    if in_braces && field(tokens.get(i)) {
        let before = i.checked_sub(1).map(|p| &tokens[p].tok);
        let after = tokens.get(i + 1).map(|t| &t.tok);
        if matches!(before, Some(Tok::Sym('{' | ',')))
            && matches!(after, Some(Tok::Sym(',' | '}' | ':')))
        {
            return Some(tokens[i].line);
        }
    }
    None
}

fn scan_source(source: &Source, fns: &mut Vec<FnInfo>) {
    let code = blank_non_code(&source.text);
    let tokens = tokenize(&code);
    let info = |name: &str| FnInfo {
        file: source.path.clone(),
        name: name.to_string(),
        reads: Vec::new(),
        code: String::new(),
    };
    // Token index of a body's `{` -> index into `fns`; open bodies as (fns index, depth, `{`).
    let mut bodies: std::collections::BTreeMap<usize, usize> = std::collections::BTreeMap::new();
    let mut open: Vec<(usize, usize, usize)> = Vec::new();
    let mut outside: Option<usize> = None;
    let mut brackets: Vec<char> = Vec::new();
    let mut depth = 0usize;
    let mut i = 0;
    while i < tokens.len() {
        if let Some(after) = skip_cfg_test_item(&tokens, i) {
            i = after;
            continue;
        }
        match tokens[i].tok {
            Tok::Sym(c @ ('{' | '(' | '[')) => {
                brackets.push(c);
                if c == '{' {
                    depth += 1;
                    if let Some(&f) = bodies.get(&i) {
                        open.push((f, depth, i));
                    }
                }
            }
            Tok::Sym('}') => {
                brackets.pop();
                if let Some(&(f, d, start)) = open.last()
                    && d == depth
                {
                    fns[f].code = join_tokens(&tokens[start..=i]);
                    open.pop();
                }
                depth = depth.saturating_sub(1);
            }
            Tok::Sym(')' | ']') => {
                brackets.pop();
            }
            _ => {}
        }
        if tokens[i].tok == Tok::Word("fn".to_string())
            && let Some(Tok::Word(name)) = tokens.get(i + 1).map(|t| &t.tok)
            && let Some(brace) = body_open(&tokens, i + 2)
        {
            fns.push(info(name));
            bodies.insert(brace, fns.len() - 1);
        }
        // The shorthand form belongs to a struct pattern or literal: inside a body, directly in braces.
        let in_braces = !open.is_empty() && brackets.last() == Some(&'{');
        if let Some(line) = read_at(&tokens, i, in_braces) {
            let owner = match open.last() {
                Some(&(f, _, _)) => f,
                None => *outside.get_or_insert_with(|| {
                    fns.push(info(OUTSIDE_FN));
                    fns.len() - 1
                }),
            };
            fns[owner].reads.push(line);
        }
        i += 1;
    }
    // A body whose `}` was never reached keeps what the tokens up to the end of the file show.
    for (f, _, start) in open {
        fns[f].code = join_tokens(&tokens[start..]);
    }
}

/// Every function of `sources` that has a body (plus one pseudo function per file with reads
/// outside any body), with the reads attributed to it.
pub fn scan(sources: &[Source]) -> Vec<FnInfo> {
    let mut fns = Vec::new();
    for source in sources {
        scan_source(source, &mut fns);
    }
    fns
}

/// Whether `code` (joined tokens) calls a function or method named `name`.
fn calls(code: &str, name: &str) -> bool {
    let call = format!("{name}(");
    code.match_indices(&call).any(|(at, _)| {
        code[..at]
            .chars()
            .next_back()
            .is_none_or(|before| !is_word(before))
    })
}

/// Every way `fns` breaks the tripwire against `table`:
/// - a reader that is not classified, or that is an admission entry point (`admit*` goes through
///   `validate` and the reachable helpers and never reads nodes or entries itself);
/// - a capability reader that lacks a needle;
/// - a table row that matches no reader (a stale classification);
/// - an admission entry point that asks for the effect modes of every entry.
pub fn problems(fns: &[FnInfo], table: &[ReaderRow]) -> Vec<String> {
    let mut out = Vec::new();
    let mut used = vec![false; table.len()];
    for f in fns.iter().filter(|f| !f.reads.is_empty()) {
        let at = format!("{}:{} fn {}", f.file, f.reads[0], f.name);
        if f.name.starts_with("admit") {
            out.push(format!(
                "{at}: an admission entry point reads plan nodes or envelope entries itself"
            ));
            continue;
        }
        let rows: Vec<usize> = table
            .iter()
            .enumerate()
            .filter(|(_, (file, name, _))| *file == f.file && *name == f.name)
            .map(|(k, _)| k)
            .collect();
        if rows.is_empty() {
            out.push(format!(
                "{at}: reads plan nodes or envelope entries and is not classified"
            ));
        }
        for k in rows {
            used[k] = true;
            if let Reads::Reachable(needles) = table[k].2 {
                for needle in needles {
                    if !f.code.contains(&normalize(needle)) {
                        out.push(format!("{at}: a capability reader lacks {needle:?}"));
                    }
                }
            }
        }
    }
    for (k, (file, name, _)) in table.iter().enumerate() {
        if !used[k] {
            out.push(format!("{file}: classified fn {name} matches no reader"));
        }
    }
    for f in fns.iter().filter(|f| f.name.starts_with("admit")) {
        if calls(&f.code, "effect_modes") {
            out.push(format!(
                "{}: fn {} asks for the effect modes of every entry",
                f.file, f.name
            ));
        }
    }
    out
}
