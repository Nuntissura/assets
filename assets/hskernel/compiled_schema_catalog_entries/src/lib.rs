#![forbid(unsafe_code)]

use std::collections::BTreeSet;

/// Inventory a compiled base schema, preserving strict table/count/duplicate checks.
pub fn compiled_schema_catalog_entries(
    schema: &str,
    expected_table_names: &[&str],
    expected_counts: (usize, usize, usize, usize, usize, usize, usize, usize),
) -> Result<Vec<String>, String> {
    let mut entries = BTreeSet::new();
    let mut tables = BTreeSet::new();
    let mut counts = [0usize; 8];
    for declaration in declarations(schema) {
        let declaration = declaration?;
        let (kind, modifier, rest) = declaration.header()?;
        let identity = match (kind, modifier) {
            ("TABLE", Modifier::Overwrite) => {
                let name = name(rest, declaration.raw)?;
                tables.insert(name.to_owned());
                if rest.windows(3).any(|tokens| words(tokens, &["TYPE", "NORMAL", "AS"])) {
                    counts[4] += 1;
                    insert_catalog_identity(&mut entries, format!("view:{name}"))?;
                }
                Some(format!("table:{name}"))
            }
            ("FIELD" | "INDEX" | "EVENT", Modifier::Overwrite) => {
                let (name, table, explicit_table) = table_target(rest, declaration.raw)?;
                if !explicit_table { continue; }
                let (prefix, count) = match kind {
                    "FIELD" => ("field", 1), "INDEX" => ("index", 2), _ => ("event", 3),
                };
                counts[count] += 1;
                Some(format!("{prefix}:{table}:{name}"))
            }
            ("SEQUENCE", Modifier::Overwrite | Modifier::IfNotExists) => {
                counts[5] += 1;
                Some(format!("sequence:{}", name(rest, declaration.raw)?))
            }
            ("ACCESS", Modifier::Overwrite | Modifier::IfNotExists)
                if words(rest.get(1..3).unwrap_or(&[]), &["ON", "DATABASE"]) => {
                counts[6] += 1;
                Some(format!("access:{}", name(rest, declaration.raw)?))
            }
            ("FUNCTION", Modifier::Overwrite) => {
                counts[7] += 1;
                Some(format!("function:{}", function_name(rest, &declaration)?))
            }
            ("ACCESS" | "FUNCTION", _) => {
                return Err(format!("unsupported declarative authority definition: {}", declaration.raw));
            }
            _ => None,
        };
        if let Some(identity) = identity { insert_catalog_identity(&mut entries, identity)?; }
    }
    let expected_tables = expected_table_names.iter().map(|name| (*name).to_owned()).collect::<BTreeSet<_>>();
    if tables != expected_tables {
        return Err(format!("declarative table inventory differs from TABLE_NAMES: parsed={}; expected={}", tables.len(), expected_tables.len()));
    }
    counts[0] = tables.len();
    let observed_counts = (counts[0], counts[1], counts[2], counts[3], counts[4], counts[5], counts[6], counts[7]);
    if observed_counts != expected_counts {
        return Err(format!("declarative schema catalog counts differ: observed={observed_counts:?}; expected={expected_counts:?}"));
    }
    Ok(entries.into_iter().collect())
}

/// Union ordered overlay identities into an already validated base inventory.
/// Overlay declarations do not inherit strict-base counts or duplicate rejection.
pub fn production_catalog_entries(
    base_entries: &[String],
    ordered_sources: &[(&str, &str)],
) -> Result<Vec<String>, String> {
    let mut entries = base_entries.iter().cloned().collect::<BTreeSet<_>>();
    for (_, source) in ordered_sources {
        for declaration in declarations(source) {
            let declaration = declaration?;
            let (kind, _, rest) = declaration.header()?;
            let identity = match kind {
                "TABLE" => format!("table:{}", name(rest, declaration.raw)?),
                "FIELD" | "INDEX" | "EVENT" => {
                    let (name, table, _) = table_target(rest, declaration.raw)?;
                    format!("{}:{table}:{name}", kind.to_ascii_lowercase())
                }
                "FUNCTION" => format!("function:{}", function_name(rest, &declaration)?),
                "SEQUENCE" | "ACCESS" | "ANALYZER" | "PARAM" => {
                    format!("{}:{}", kind.to_ascii_lowercase(), name(rest, declaration.raw)?)
                }
                _ => return Err(format!("unsupported production catalog declaration: {}", declaration.raw)),
            };
            entries.insert(identity);
        }
    }
    Ok(entries.into_iter().collect())
}

pub fn declarative_function_name(signature: &str) -> Result<&str, String> {
    let (name, _) = signature.split_once('(')
        .ok_or_else(|| format!("function declaration lacks argument opener: {signature}"))?;
    if !name.starts_with("fn::") || name.len() == 4 {
        return Err(format!("invalid declarative function name: {signature}"));
    }
    Ok(name)
}

pub fn insert_catalog_identity(entries: &mut BTreeSet<String>, identity: String) -> Result<(), String> {
    if entries.insert(identity.clone()) { Ok(()) }
    else { Err(format!("duplicate declarative schema identity: {identity}")) }
}

#[derive(Clone, Copy, PartialEq)]
enum Modifier { Plain, Overwrite, IfNotExists }

#[derive(Clone, Copy, PartialEq)]
enum TokenKind { Word, Quoted, Symbol(char) }

#[derive(Clone, Copy)]
struct Token<'a> { text: &'a str, kind: TokenKind, start: usize, end: usize }

impl Token<'_> {
    fn is_word(&self, word: &str) -> bool { self.kind == TokenKind::Word && self.text == word }
}

struct Declaration<'a> { tokens: Vec<Token<'a>>, raw: &'a str }

impl<'a> Declaration<'a> {
    fn header(&self) -> Result<(&'a str, Modifier, &[Token<'a>]), String> {
        let kind = self.tokens.get(1).filter(|token| token.kind == TokenKind::Word)
            .ok_or_else(|| format!("missing DEFINE kind: {}", self.raw))?.text;
        let mut position = 2;
        let modifier = if self.tokens.get(position).is_some_and(|token| token.is_word("OVERWRITE")) {
            position += 1; Modifier::Overwrite
        } else if self.tokens.get(position).is_some_and(|token| token.is_word("IF")) {
            if !words(self.tokens.get(position..position + 3).unwrap_or(&[]), &["IF", "NOT", "EXISTS"]) {
                return Err(format!("invalid declaration modifier: {}", self.raw));
            }
            position += 3; Modifier::IfNotExists
        } else { Modifier::Plain };
        Ok((kind, modifier, &self.tokens[position..]))
    }
}

fn words(tokens: &[Token<'_>], expected: &[&str]) -> bool {
    tokens.len() == expected.len() && tokens.iter().zip(expected).all(|(token, word)| token.is_word(word))
}

fn name<'a>(tokens: &[Token<'a>], raw: &str) -> Result<&'a str, String> {
    tokens.first().filter(|token| matches!(token.kind, TokenKind::Word | TokenKind::Quoted)
        && !token.is_word("DEFINE"))
        .map(|token| token.text)
        .ok_or_else(|| format!("missing DEFINE name: {raw}"))
}

fn table_target<'a>(tokens: &[Token<'a>], raw: &str) -> Result<(&'a str, &'a str, bool), String> {
    let field = name(tokens, raw)?;
    if !tokens.get(1).is_some_and(|token| token.is_word("ON")) {
        return Err(format!("invalid catalog declaration: {raw}"));
    }
    let explicit_table = tokens.get(2).is_some_and(|token| token.is_word("TABLE"));
    let table = tokens.get(if explicit_table { 3 } else { 2 })
        .filter(|token| matches!(token.kind, TokenKind::Word | TokenKind::Quoted)
            && !(token.kind == TokenKind::Word && ["DEFINE", "ON", "TABLE", "TYPE", "WHEN", "THEN", "FIELDS", "ASSERT", "PERMISSIONS"].contains(&token.text)))
        .ok_or_else(|| format!("missing declaration table: {raw}"))?.text;
    Ok((field, table, explicit_table))
}

fn function_name<'a>(tokens: &[Token<'a>], declaration: &Declaration<'a>) -> Result<&'a str, String> {
    let name = name(tokens, declaration.raw)?;
    let offset = tokens[0].start - declaration.tokens[0].start;
    let signature = declaration.raw[offset..].split_whitespace().next().unwrap_or(name);
    if tokens.get(1).map(|token| token.kind) != Some(TokenKind::Symbol('(')) {
        return Err(format!("function declaration lacks argument opener: {name}"));
    }
    if signature.contains('(') || !name.starts_with("fn::") || name.len() == 4 {
        declarative_function_name(signature)?;
    } else {
        let end = tokens[1].end - declaration.tokens[0].start;
        declarative_function_name(&declaration.raw[offset..end])?;
    }
    Ok(name)
}

struct Declarations<'a> {
    lexer: Lexer<'a>,
    tokens: Vec<Token<'a>>,
    delimiters: Vec<char>,
    start: Option<usize>,
    end: usize,
    finished: bool,
}

fn declarations(source: &str) -> Declarations<'_> {
    Declarations { lexer: Lexer { source, position: 0, expects_value: true }, tokens: Vec::new(),
        delimiters: Vec::new(), start: None, end: 0, finished: false }
}

impl<'a> Iterator for Declarations<'a> {
    type Item = Result<Declaration<'a>, String>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.finished { return None; }
        loop {
            let token = match self.lexer.next() {
                Ok(Some(token)) => token,
                Err(error) => { self.finished = true; return Some(Err(error)); }
                Ok(None) => {
                    self.finished = true;
                    if !self.delimiters.is_empty() {
                        return Some(Err("unterminated declaration delimiter".to_owned()));
                    }
                    return self.take_declaration();
                }
            };
            self.start.get_or_insert(token.start);
            self.end = token.end;
            match token.kind {
                TokenKind::Symbol('(' | '[' | '{') => {
                    if self.delimiters.is_empty() { self.tokens.push(token); }
                    if let TokenKind::Symbol(symbol) = token.kind { self.delimiters.push(symbol); }
                }
                TokenKind::Symbol(')' | ']' | '}') => {
                    let expected = match token.kind { TokenKind::Symbol(')') => '(', TokenKind::Symbol(']') => '[', _ => '{' };
                    if self.delimiters.pop() != Some(expected) {
                        self.finished = true;
                        return Some(Err(format!("unmatched delimiter at byte {}", token.start)));
                    }
                    if expected == '[' && self.delimiters.is_empty() && self.in_field_name() {
                        self.tokens.push(token);
                    }
                }
                TokenKind::Symbol(';') if self.delimiters.is_empty() => {
                    if let Some(declaration) = self.take_declaration() { return Some(declaration); }
                }
                _ if self.delimiters.is_empty() => self.tokens.push(token),
                _ => {}
            }
        }
    }
}

impl<'a> Declarations<'a> {
    fn in_field_name(&self) -> bool {
        self.tokens.get(1).is_some_and(|token| token.is_word("FIELD"))
            && !self.tokens.iter().any(|token| token.is_word("ON"))
    }

    fn take_declaration(&mut self) -> Option<Result<Declaration<'a>, String>> {
        let start = self.start.take()?;
        if self.tokens.first().is_some_and(|token| token.is_word("DEFINE")) {
            if self.tokens.get(1).is_some_and(|token| token.is_word("FIELD")) {
                let first = match self.tokens.get(2) {
                    Some(token) if token.is_word("OVERWRITE") => 3,
                    Some(token) if token.is_word("IF") => 5,
                    _ => 2,
                };
                if let Some(on) = self.tokens.iter().position(|token| token.is_word("ON")) {
                    if self.tokens[first.min(on)..on].iter().any(|token| token.is_word("DEFINE")) {
                        return Some(Err(format!("invalid catalog declaration: {}", self.lexer.source[start..self.end].trim())));
                    }
                    if on > first + 1 {
                        let begin = self.tokens[first].start;
                        let end = self.tokens[on - 1].end;
                        let token = Token { text: &self.lexer.source[begin..end], kind: TokenKind::Word, start: begin, end };
                        self.tokens.splice(first..on, [token]);
                    }
                }
            }
            Some(Ok(Declaration { tokens: std::mem::take(&mut self.tokens),
                raw: self.lexer.source[start..self.end].trim() }))
        } else { self.tokens.clear(); None }
    }
}
struct Lexer<'a> { source: &'a str, position: usize, expects_value: bool }

impl<'a> Lexer<'a> {
    fn peek(&self) -> Option<char> { self.source[self.position..].chars().next() }
    fn bump(&mut self) -> Option<char> {
        let next = self.peek()?; self.position += next.len_utf8(); Some(next)
    }
    fn starts(&self, text: &str) -> bool { self.source[self.position..].starts_with(text) }
    fn next(&mut self) -> Result<Option<Token<'a>>, String> {
        loop {
            while self.peek().is_some_and(char::is_whitespace) { self.bump(); }
            if self.starts("--") || self.starts("//") {
                while self.peek().is_some_and(|character| character != '\n') { self.bump(); }
            } else if self.starts("/*") {
                let start = self.position;
                self.position += 2;
                let mut depth = 1usize;
                while depth != 0 {
                    if self.starts("/*") { self.position += 2; depth += 1; }
                    else if self.starts("*/") { self.position += 2; depth -= 1; }
                    else if self.bump().is_none() { return Err(format!("unterminated block comment at byte {start}")); }
                }
            } else { break; }
        }
        let Some(character) = self.peek() else { return Ok(None); };
        let start = self.position;
        let kind = if matches!(character, '\'' | '"' | '`' | '⟨') {
            let closing = if character == '⟨' { '⟩' } else { character };
            self.bump();
            loop {
                match self.bump() {
                    Some('\\') => { if self.bump().is_none() { return Err(format!("unterminated quote at byte {start}")); } }
                    Some(value) if value == closing => break,
                    Some(_) => {}
                    None => return Err(format!("unterminated quote at byte {start}")),
                }
            }
            TokenKind::Quoted
        } else if character == '/' && self.expects_value {
            self.bump();
            let mut class = false;
            loop {
                match self.bump() {
                    Some('\\') => { if self.bump().is_none() { return Err(format!("unterminated regex at byte {start}")); } }
                    Some('[') => class = true,
                    Some(']') => class = false,
                    Some('/') if !class => break,
                    Some(_) => {}
                    None => return Err(format!("unterminated regex at byte {start}")),
                }
            }
            TokenKind::Quoted
        } else if matches!(character, '(' | ')' | '[' | ']' | '{' | '}' | ';' | ',' | '/') {
            self.bump(); TokenKind::Symbol(character)
        } else {
            while self.peek().is_some_and(|value| !value.is_whitespace()
                && !matches!(value, '(' | ')' | '[' | ']' | '{' | '}' | ';' | ',' | '/' | '\'' | '"' | '`' | '⟨'))
                && !self.starts("--") { self.bump(); }
            TokenKind::Word
        };
        let text = &self.source[start..self.position];
        self.expects_value = matches!(kind, TokenKind::Symbol('(' | '[' | '{' | ',' | ';' | '/'))
            || (kind == TokenKind::Word && (text.ends_with('~') || text.ends_with('=')
                || ["RETURN", "SELECT", "ASSERT", "VALUE", "DEFAULT", "WHEN", "THEN", "AND", "OR", "NOT", "IN", "=", "~", "+", "-", "*"].contains(&text)));
        Ok(Some(Token { text, kind, start, end: self.position }))
    }
}
