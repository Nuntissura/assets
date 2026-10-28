use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;

/// Interaction purpose, independent of the host retrieval implementation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueryPurpose {
    #[default]
    SearchWrite,
    Navigation,
    Commands,
    Settings,
}

/// Bounded common query grammar. Hosts normalize Unicode to NFC before parsing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParsedQuery {
    pub words: Vec<String>,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub result_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    #[serde(rename = "in", skip_serializing_if = "Option::is_none")]
    pub area: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    pub explanation: String,
    pub incomplete: bool,
    pub unsupported: Vec<String>,
    pub purpose: QueryPurpose,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueryError { TextLimit, TokenLimit }
impl std::fmt::Display for QueryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(f, "{self:?}") }
}
impl std::error::Error for QueryError {}

/// Parse literal text without executing commands or treating unsupported syntax as empty success.
pub fn parse_query(text: &str) -> Result<ParsedQuery, QueryError> {
    if text.len() > 65536 { return Err(QueryError::TextLimit); }
    let trimmed = text.trim_start();
    let (input, purpose) = if let Some(rest) = trimmed.strip_prefix('>') {
        (rest, QueryPurpose::Commands)
    } else { (text, QueryPurpose::SearchWrite) };
    let mut parsed = ParsedQuery { words: Vec::new(), result_type: None, tag: None, area: None,
        task: None, explanation: String::new(), incomplete: false, unsupported: Vec::new(), purpose };
    let mut tokens = Vec::new();
    let mut token = String::new();
    let mut quoted = false;
    let mut escaped = false;
    for ch in input.chars() {
        if quoted && escaped { escaped = false; token.push(ch); }
        else if quoted && ch == '\\' { escaped = true; token.push(ch); }
        else if ch == '"' { quoted = !quoted; token.push(ch); }
        else if ch.is_whitespace() && !quoted {
            if !token.is_empty() { tokens.push(std::mem::take(&mut token)); }
        } else { token.push(ch); }
        if tokens.len() > 1024 { return Err(QueryError::TokenLimit); }
    }
    if !token.is_empty() { tokens.push(token); }
    if tokens.len() > 1024 { return Err(QueryError::TokenLimit); }
    parsed.incomplete = quoted;
    let mut lookup_syntax = false;
    let token_count = tokens.len();
    for (index, token) in tokens.into_iter().enumerate() {
        let partial = quoted && index + 1 == token_count;
        if token.starts_with('"') {
            let Some(word) = query_text(&token, partial) else {
                parsed.incomplete = true; parsed.unsupported.push(token.clone());
                parsed.words.push(token.to_lowercase()); continue;
            };
            let word = word.to_lowercase();
            if !word.is_empty() { parsed.words.push(word); }
            if token.len() == 1 { parsed.incomplete = true; }
            continue;
        }
        let filter = token.split_once(':').filter(|(field, _)|
            ["type", "tag", "in", "is"].iter().any(|name| field.eq_ignore_ascii_case(name)));
        let Some((field, raw)) = filter else { parsed.words.push(token.to_lowercase()); continue; };
        lookup_syntax = true;
        let field = field.to_ascii_lowercase();
        let Some(value) = query_text(raw, partial) else {
            parsed.incomplete = true; parsed.unsupported.push(token.clone());
            parsed.words.push(token.to_lowercase()); continue;
        };
        let value = value.to_lowercase();
        if value.is_empty() { parsed.incomplete = true; continue; }
        let supported = match field.as_str() {
            "type" => ["note", "notes", "heading", "block", "task", "tag", "settings", "manual", "navigation", "command", "feed", "file", "folder"].contains(&value.as_str()),
            "in" => ["title", "body"].contains(&value.as_str()),
            "is" => ["todo", "doing", "done", "read", "unread", "bookmarked"].contains(&value.as_str()),
            _ => true,
        };
        if !supported {
            parsed.unsupported.push(token.clone()); parsed.words.push(token.to_lowercase());
            parsed.incomplete = true; continue;
        }
        match field.as_str() {
            "type" => parsed.result_type = Some(value),
            "tag" => parsed.tag = Some(if raw.trim_matches('"').starts_with('#') {
                value.strip_prefix('#').unwrap_or(&value).to_owned()
            } else { value }),
            "in" => parsed.area = Some(value),
            _ => parsed.task = Some(value),
        }
    }
    if parsed.purpose != QueryPurpose::Commands {
        parsed.purpose = match parsed.result_type.as_deref() {
            Some("command") => QueryPurpose::Commands,
            Some("settings") => QueryPurpose::Settings,
            Some(_) => QueryPurpose::Navigation,
            None if parsed.tag.is_some() || parsed.area.is_some() || parsed.task.is_some() => QueryPurpose::Navigation,
            None => QueryPurpose::SearchWrite,
        };
    }
    // Even unfinished operators are intentional lookup syntax, never ordinary prose.
    if lookup_syntax && parsed.incomplete && parsed.purpose == QueryPurpose::SearchWrite { parsed.purpose = QueryPurpose::Navigation; }
    let mut explanation = Vec::new();
    if let Some(value) = &parsed.result_type { explanation.push(format!("type {value}")); }
    if let Some(value) = &parsed.tag { explanation.push(format!("tag {value}")); }
    if let Some(value) = &parsed.area { explanation.push(format!("in {value}")); }
    if let Some(value) = &parsed.task { explanation.push(format!("task {value}")); }
    if !parsed.words.is_empty() { explanation.push("all words / exact quoted phrases".into()); }
    parsed.explanation = if explanation.is_empty() { "Browse authorized results".into() } else { explanation.join(" · ") };
    Ok(parsed)
}

// Decode only complete quoted values. Unquoted backslashes and partial-query text stay literal.
fn query_text(raw: &str, partial: bool) -> Option<String> {
    if partial || !raw.starts_with('"') || !raw.contains('\\') {
        return Some(raw.trim_matches('"').to_owned());
    }
    let mut chars = raw.strip_prefix('"')?.strip_suffix('"')?.chars();
    let mut text = String::new();
    while let Some(ch) = chars.next() {
        if ch != '\\' { text.push(ch); continue; }
        text.push(match chars.next()? {
            '"' => '"', '\\' => '\\', '/' => '/', 'b' => '\u{0008}', 'f' => '\u{000c}',
            'n' => '\n', 'r' => '\r', 't' => '\t',
            'u' => {
                let first = hex_quad(&mut chars)?;
                let scalar = if (0xd800..=0xdbff).contains(&first) {
                    if chars.next()? != '\\' || chars.next()? != 'u' { return None; }
                    let low = hex_quad(&mut chars)?;
                    if !(0xdc00..=0xdfff).contains(&low) { return None; }
                    0x10000 + ((first - 0xd800) << 10) + low - 0xdc00
                } else { first };
                char::from_u32(scalar)?
            }
            _ => return None,
        });
    }
    Some(text.nfc().collect())
}
fn hex_quad(chars: &mut std::str::Chars<'_>) -> Option<u32> {
    let mut value = 0;
    for _ in 0..4 { value = value * 16 + chars.next()?.to_digit(16)?; }
    Some(value)
}
