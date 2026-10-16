//! CSS text of hostile SVG: style attribute declarations and `url()` functions, tokenised per CSS
//! Syntax Level 3 (comments removed, strings, nesting and escapes honoured) instead of matching
//! substrings of the raw text. Property names and function names compare ASCII case-insensitively.
use std::iter::Peekable;
use std::str::Chars;

/// Deepest bracket nesting the declaration scanner tracks; deeper openers are plain characters.
const MAX_NESTING: usize = 32;
/// Stands in for an escaped character that cannot occur in an identifier, so a value like
/// `fill:\28red` cannot become a function or delimiter after decoding; consumers reject it.
const NOT_AN_IDENT: char = '\u{FFFD}';

fn is_ws(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r' | '\x0c')
}

fn is_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-' || c == '_' || !c.is_ascii()
}

/// Reads the character(s) after a consumed `\`: 1 to 6 hex digits plus one optional whitespace,
/// or any single character. `None` for a backslash followed by a newline (not an escape) and at
/// the end of input.
fn escape(chars: &mut Peekable<Chars<'_>>) -> Option<char> {
    let first = *chars.peek()?;
    if matches!(first, '\n' | '\r' | '\x0c') {
        return None;
    }
    if !first.is_ascii_hexdigit() {
        chars.next();
        return Some(first);
    }
    let mut value: u32 = 0;
    let mut digits = 0u8;
    while digits < 6 {
        let Some(d) = chars.peek().and_then(|c| c.to_digit(16)) else {
            break;
        };
        value = value.saturating_mul(16).saturating_add(d);
        digits = digits.saturating_add(1);
        chars.next();
    }
    if let Some(&c) = chars.peek().filter(|c| is_ws(**c)) {
        chars.next();
        if c == '\r' && chars.peek() == Some(&'\n') {
            chars.next();
        }
    }
    Some(match char::from_u32(value) {
        Some(c) if value != 0 => c,
        _ => NOT_AN_IDENT,
    })
}

/// Splits a `style` attribute into `(property, value)` pairs in source order. Comments are
/// dropped, `;` and `:` inside strings, brackets and `url(...)` tokens do not split, escapes
/// outside strings are decoded, the property name is lowercased and `!important` is removed.
/// Strings and bare `url(` tokens are kept verbatim (escapes included) for [`url_function`].
pub(super) fn declarations(style: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut name: Option<String> = None;
    let mut cur = String::new();
    let mut closers: Vec<char> = Vec::new();
    let mut chars = style.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                while let Some(d) = chars.next() {
                    if d == '*' && chars.peek() == Some(&'/') {
                        chars.next();
                        break;
                    }
                }
                cur.push(' ');
            }
            '"' | '\'' => {
                cur.push(c);
                copy_string(c, &mut chars, &mut cur);
            }
            '\\' => cur.push(match escape(&mut chars) {
                Some(d) if is_ident_char(d) => d,
                _ => NOT_AN_IDENT,
            }),
            '(' => {
                cur.push(c);
                if ends_with_url(&cur) && !quoted_next(&chars) {
                    copy_bare_url(&mut chars, &mut cur);
                } else if closers.len() < MAX_NESTING {
                    closers.push(')');
                }
            }
            '[' | '{' => {
                cur.push(c);
                if closers.len() < MAX_NESTING {
                    closers.push(if c == '[' { ']' } else { '}' });
                }
            }
            ')' | ']' | '}' => {
                cur.push(c);
                if closers.last() == Some(&c) {
                    closers.pop();
                }
            }
            ':' if closers.is_empty() && name.is_none() => {
                name = Some(std::mem::take(&mut cur));
            }
            ';' if closers.is_empty() => {
                finish(name.take(), std::mem::take(&mut cur), &mut out);
            }
            _ => cur.push(c),
        }
    }
    finish(name, cur, &mut out);
    out
}

/// Copies a string token (opening quote already pushed) verbatim; an unescaped newline ends it
/// as a bad string (CSS Syntax 4.3.5).
fn copy_string(quote: char, chars: &mut Peekable<Chars<'_>>, cur: &mut String) {
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                cur.push(c);
                if let Some(d) = chars.next() {
                    cur.push(d);
                }
            }
            '\n' | '\r' | '\x0c' => {
                cur.push(c);
                return;
            }
            _ => {
                cur.push(c);
                if c == quote {
                    return;
                }
            }
        }
    }
}

fn ends_with_url(cur: &str) -> bool {
    let Some(head) = cur.strip_suffix('(') else {
        return false;
    };
    let tail: Vec<char> = head.chars().rev().take(4).collect();
    let word: String = tail.iter().take(3).rev().collect();
    word.eq_ignore_ascii_case("url") && !tail.get(3).is_some_and(|c| is_ident_char(*c))
}

/// After `url(`: does a quote follow (so the argument is a string, not a bare url token)?
fn quoted_next(chars: &Peekable<Chars<'_>>) -> bool {
    chars
        .clone()
        .find(|c| !is_ws(*c))
        .is_some_and(|c| c == '"' || c == '\'')
}

/// Copies a bare url token up to and including its `)` verbatim; `\)` does not end it.
fn copy_bare_url(chars: &mut Peekable<Chars<'_>>, cur: &mut String) {
    while let Some(c) = chars.next() {
        cur.push(c);
        match c {
            '\\' => {
                if let Some(d) = chars.next() {
                    cur.push(d);
                }
            }
            ')' => return,
            _ => {}
        }
    }
}

fn finish(name: Option<String>, value: String, out: &mut Vec<(String, String)>) {
    let Some(name) = name else {
        return;
    };
    let name = name.trim().to_ascii_lowercase();
    if name.is_empty() || !name.chars().all(is_ident_char) {
        return;
    }
    let value = value.trim();
    let value = strip_important(value);
    out.push((name, value.to_owned()));
}

/// Removes a trailing `!important` (`!` then optional whitespace then the keyword, any case).
fn strip_important(value: &str) -> &str {
    let Some(bang) = value.rfind('!') else {
        return value;
    };
    if value[bang..]
        .strip_prefix('!')
        .is_some_and(|rest| rest.trim().eq_ignore_ascii_case("important"))
    {
        value[..bang].trim_end()
    } else {
        value
    }
}

/// Where a `url(...)` points.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum UrlTarget {
    /// `#id`, the only kind this crate resolves.
    Local(String),
    /// Anything else (a file, a network address, a `data:` URL): never fetched.
    External,
    /// A bad string or bad url token, or an empty fragment.
    Malformed,
}

/// A `url()` function at the start of a value, with the text after its closing parenthesis.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct UrlFunction {
    pub target: UrlTarget,
    pub rest: String,
}

/// Parses a leading `url(...)` function: the function name is read as an identifier (escapes
/// decoded, any case), the argument is a string or a bare url token with escapes decoded, and
/// surrounding whitespace is ignored. `None` when the value does not start with `url(`.
pub(super) fn url_function(value: &str) -> Option<UrlFunction> {
    let mut chars = value.trim_start_matches(is_ws).chars().peekable();
    let mut name = String::new();
    while let Some(&c) = chars.peek() {
        if c == '\\' {
            chars.next();
            name.push(escape(&mut chars)?);
        } else if is_ident_char(c) {
            name.push(c);
            chars.next();
        } else {
            break;
        }
    }
    if !name.eq_ignore_ascii_case("url") || chars.next() != Some('(') {
        return None;
    }
    while chars.peek().is_some_and(|c| is_ws(*c)) {
        chars.next();
    }
    let mut target = String::new();
    let mut ok = true;
    match chars.peek().copied() {
        Some(quote @ ('"' | '\'')) => {
            chars.next();
            loop {
                match chars.next() {
                    None => {
                        ok = false;
                        break;
                    }
                    Some(c) if c == quote => break,
                    Some('\n' | '\r' | '\x0c') => {
                        ok = false;
                        break;
                    }
                    Some('\\') => {
                        // A backslash-newline is a line continuation inside a string.
                        if matches!(chars.peek(), Some('\n' | '\r' | '\x0c')) {
                            chars.next();
                        } else if let Some(d) = escape(&mut chars) {
                            target.push(d);
                        }
                    }
                    Some(c) => target.push(c),
                }
            }
        }
        _ => loop {
            match chars.peek().copied() {
                None | Some(')') => break,
                Some(c) if is_ws(c) => break,
                Some('"' | '\'' | '(') => {
                    ok = false;
                    chars.next();
                }
                Some(c) if c.is_control() => {
                    ok = false;
                    chars.next();
                }
                Some('\\') => {
                    chars.next();
                    match escape(&mut chars) {
                        Some(d) => target.push(d),
                        None => ok = false,
                    }
                }
                Some(c) => {
                    target.push(c);
                    chars.next();
                }
            }
        },
    }
    while chars.peek().is_some_and(|c| is_ws(*c)) {
        chars.next();
    }
    if chars.next() != Some(')') {
        return Some(UrlFunction {
            target: UrlTarget::Malformed,
            rest: String::new(),
        });
    }
    let rest: String = chars.collect();
    let rest = rest.trim().to_owned();
    let target = target.trim_matches(is_ws);
    let target = if !ok {
        UrlTarget::Malformed
    } else if let Some(fragment) = target.strip_prefix('#') {
        if fragment.is_empty() || fragment.contains(is_ws) {
            UrlTarget::Malformed
        } else {
            UrlTarget::Local(fragment.to_owned())
        }
    } else {
        UrlTarget::External
    };
    Some(UrlFunction { target, rest })
}
