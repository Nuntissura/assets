//! Signature argument parsing and active-parameter counting.
//! Preserves the existing scanner heuristics and byte-wise label parsing.

pub fn top_level_paren_args(label: &str) -> Option<Vec<String>> {
    let bytes = label.as_bytes();
    // Locate the first top-level '(' (depth 0 in the OTHER bracket kinds, ignoring string/char state).
    let mut scanner = DepthScanner::new();
    let mut open: Option<usize> = None;
    for (i, &b) in bytes.iter().enumerate() {
        let c = b as char;
        if !scanner.in_literal() && scanner.paren_depth() == 0 && c == '(' {
            open = Some(i);
            break;
        }
        scanner.step(c);
    }
    let open = open?;
    // Walk from just after '(' collecting args, splitting at depth-1 commas, until the matching ')'.
    let mut args: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut inner = DepthScanner::new();
    inner.step('('); // enter the call: paren_depth now 1.
    let mut closed = false;
    for &b in &bytes[open + 1..] {
        let c = b as char;
        // The matching close-paren is the first ')' at the call's own paren depth (1), outside a
        // literal — a nested ')' (depth > 1) just decrements and stays inside the arg.
        if !inner.in_literal() && inner.paren_depth() == 1 && c == ')' {
            closed = true;
            break;
        }
        // A top-level argument separator is a ',' at the call's top level (no nested []/{}/<>) — a comma
        // inside a generic/array/map/nested-call stays part of the current arg.
        if c == ',' && inner.at_call_top_level() {
            let trimmed = current.trim().to_owned();
            if !trimmed.is_empty() {
                args.push(trimmed);
            }
            current.clear();
            inner.step(c);
            continue;
        }
        current.push(c);
        inner.step(c);
    }
    if !closed {
        // Unbalanced display name; still treat what we have as a best-effort arg list.
    }
    let trimmed = current.trim().to_owned();
    if !trimmed.is_empty() {
        args.push(trimmed);
    }
    Some(args)
}

/// Count the TOP-LEVEL commas between the call's open-paren and the cursor, returning the active
/// parameter index (commas-before-cursor at the call's depth). `buffer_slice` is the text from
/// `open_paren_byte` (INCLUSIVE — its first char should be the `(`) to `cursor_byte`. Commas nested in
/// inner `()`/`[]`/`{}`/generic `<>` brackets, string literals, and char literals are SKIPPED
/// (RISK-001 / MC-001 / AC-007). The result is 0 right after the open-paren and increments at each
/// top-level comma the cursor has passed.
///
/// `open_paren_byte` and `cursor_byte` are absolute buffer offsets; the slice must be exactly
/// `buffer[open_paren_byte..cursor_byte]` (the panel slices it). When the slice does not start with `(`
/// the scanner still works (it just never enters the call depth and returns 0), so a defensive caller
/// never panics.
pub fn active_parameter_from_commas(
    buffer_slice: &str,
    open_paren_byte: usize,
    cursor_byte: usize,
) -> usize {
    // `open_paren_byte` / `cursor_byte` are documented for the caller's contract; the count is computed
    // purely from the slice (the offsets let the caller assert the slice bounds).
    debug_assert!(open_paren_byte <= cursor_byte);
    let mut scanner = DepthScanner::new();
    let mut commas = 0usize;
    // The slice begins at the '(': enter the call so its arguments are at depth 1.
    let mut chars = buffer_slice.chars();
    // Consume the leading '(' if present so the call's base depth is 1.
    if let Some(first) = chars.clone().next() {
        if first == '(' {
            scanner.step('(');
            chars.next();
        }
    }
    for c in chars {
        // A comma at the call's top argument level (paren depth 1, no nested []/{}/<> and not inside a
        // literal) is a top-level argument separator (RISK-001 / AC-007).
        if c == ',' && scanner.at_call_top_level() {
            commas += 1;
        }
        scanner.step(c);
    }
    commas
}

/// A small depth-aware scanner over source-like text that tracks bracket nesting and string/char
/// literal state, so a comma scan skips commas nested inside `()`/`[]`/`{}`/generic `<>` brackets and
/// inside string/char literals (RISK-001 / MC-001). It is intentionally lightweight (no full lexer):
/// it tracks paren/bracket/brace/angle depth and a string/char literal flag with backslash escaping.
/// `<` only increments angle depth in a generic-looking context (preceded by an identifier char or
/// `>`); otherwise (e.g. a `<` comparison) it is treated as plain text so an expression like `a < b`
/// does not wrongly enter a generic depth.
#[derive(Debug, Clone)]
struct DepthScanner {
    paren: i32,
    bracket: i32,
    brace: i32,
    angle: i32,
    in_string: bool,
    in_char: bool,
    escaped: bool,
    prev: Option<char>,
}

impl DepthScanner {
    fn new() -> Self {
        Self {
            paren: 0,
            bracket: 0,
            brace: 0,
            angle: 0,
            in_string: false,
            in_char: false,
            escaped: false,
            prev: None,
        }
    }

    /// Whether the scanner is currently inside a string or char literal.
    fn in_literal(&self) -> bool {
        self.in_string || self.in_char
    }

    /// The current `()` nesting depth (the call-argument depth the comma scanner counts at).
    fn paren_depth(&self) -> i32 {
        self.paren
    }

    /// Whether the scanner is exactly at the call's top argument level: paren depth 1 and NO open
    /// `[]`/`{}`/`<>` nesting and not inside a literal. A comma here is a top-level argument separator;
    /// a comma anywhere else (a nested call/array/map/generic/literal) is NOT (RISK-001 / AC-007).
    fn at_call_top_level(&self) -> bool {
        !self.in_literal()
            && self.paren == 1
            && self.bracket == 0
            && self.brace == 0
            && self.angle == 0
    }

    /// Advance the scanner over one character, updating literal + depth state.
    fn step(&mut self, c: char) {
        // Inside a string/char literal: only the matching unescaped quote (or end-escape) matters.
        if self.in_string {
            if self.escaped {
                self.escaped = false;
            } else if c == '\\' {
                self.escaped = true;
            } else if c == '"' {
                self.in_string = false;
            }
            self.prev = Some(c);
            return;
        }
        if self.in_char {
            if self.escaped {
                self.escaped = false;
            } else if c == '\\' {
                self.escaped = true;
            } else if c == '\'' {
                self.in_char = false;
            }
            self.prev = Some(c);
            return;
        }
        match c {
            '"' => self.in_string = true,
            '\'' => self.in_char = true,
            '(' => self.paren += 1,
            ')' => self.paren = (self.paren - 1).max(0),
            '[' => self.bracket += 1,
            ']' => self.bracket = (self.bracket - 1).max(0),
            '{' => self.brace += 1,
            '}' => self.brace = (self.brace - 1).max(0),
            '<' => {
                // Only treat '<' as a generic open when it looks like a generic (preceded by an
                // identifier char or a closing '>'), so a comparison `a < b` is not misread.
                if matches!(self.prev, Some(p) if p.is_alphanumeric() || p == '_' || p == '>') {
                    self.angle += 1;
                }
            }
            '>' if self.angle > 0 => {
                // Close only an open generic; arrows and comparisons cannot underflow.
                self.angle -= 1;
            }
            _ => {}
        }
        self.prev = Some(c);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_parameter_counts_top_level_commas() {
        // `(a, b, c)` — cursor after the second top-level comma -> active parameter 2.
        let s = "(a, b, c";
        assert_eq!(active_parameter_from_commas(s, 0, s.len()), 2);
        // Right after the open-paren -> parameter 0.
        assert_eq!(active_parameter_from_commas("(", 0, 1), 0);
        // After the first comma -> parameter 1.
        assert_eq!(active_parameter_from_commas("(a,", 0, 3), 1);
    }

    #[test]
    fn active_parameter_ignores_nested_calls() {
        // The comma inside `inner(x, y)` must NOT advance the outer active parameter (RISK-001).
        let s = "(a, inner(x, y), ";
        // Outer commas the cursor has passed: after `a,` (1) and after `inner(...),` (2) -> param 2.
        assert_eq!(active_parameter_from_commas(s, 0, s.len()), 2);
        // Cursor INSIDE the inner call, after its first comma: outer param is still 1 (the inner comma
        // is at depth 2, skipped).
        let inside = "(a, inner(x, ";
        assert_eq!(active_parameter_from_commas(inside, 0, inside.len()), 1);
    }

    #[test]
    fn active_parameter_ignores_brackets_braces_and_generics() {
        // Commas inside `[]`, `{}`, and generic `<>` are skipped (AC-007).
        let arr = "(a, [1, 2, 3], ";
        assert_eq!(active_parameter_from_commas(arr, 0, arr.len()), 2);
        let map = "(a, Foo { x: 1, y: 2 }, ";
        assert_eq!(active_parameter_from_commas(map, 0, map.len()), 2);
        let generic = "(a, HashMap<K, V>, ";
        assert_eq!(active_parameter_from_commas(generic, 0, generic.len()), 2);
    }

    #[test]
    fn active_parameter_ignores_string_and_char_literals() {
        // Commas inside a string literal and a char literal must be skipped (AC-007).
        let strs = "(a, \"x, y, z\", ";
        assert_eq!(active_parameter_from_commas(strs, 0, strs.len()), 2);
        let chr = "(a, ',', ";
        assert_eq!(active_parameter_from_commas(chr, 0, chr.len()), 2);
        // An escaped quote inside a string does not end the literal early.
        let esc = "(a, \"he said \\\"hi, there\\\"\", ";
        assert_eq!(active_parameter_from_commas(esc, 0, esc.len()), 2);
    }

    #[test]
    fn comparison_less_than_is_not_a_generic() {
        // `a < b` is a comparison, not a generic open, so the following comma stays top-level.
        let s = "(a < b, c";
        assert_eq!(active_parameter_from_commas(s, 0, s.len()), 1);
    }

}
