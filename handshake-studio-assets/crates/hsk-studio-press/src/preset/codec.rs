//! Canonical text codec of a [`PdfPreset`] (lane SMB-PDF-STOCK-PRESETS-1, MT-30842, STU-LAY-168
//! save/load round trip). Hand-written, no dependency.
//!
//! Format (UTF-8, `\n` line ends, deterministic: settings in [`SETTINGS`] order, descriptions in
//! map order, reals in Rust's shortest round-trip form):
//!
//! ```text
//! hsk.studio.pdf_preset@1
//! id "hsk.studio.pdf_preset.press_quality"
//! name "Press Quality"
//! source shipped                  | operator ["<parent>"] | imported "<origin>"
//! default_locale "en"
//! description "en" "<text>"
//! setting general.compatibility_level enum "1.4"
//! setting general.start_page int 1
//! setting colour_images.downsample_threshold real 1.5
//! setting fonts.always_embed fonts 2 "Inter" "Inter-Bold"
//! setting colour_images.auto_colour_space_quality_dictionary quality 1 "QFactor" num 0.15
//! setting output_intent.output_intent_profile text ""
//! end
//! ```
//!
//! Hostile text (V4-01/02, V4-08): the input size, line count, line length, token count and every
//! decoded value are bounded before allocation; each line is tokenised (bare words and quoted
//! strings, escapes `\\ \" \n \r \t \u{hex}` decoded first) and only whole tokens are compared,
//! case-sensitively; a line whose first non-blank character is `#` is a comment. Directives, keys,
//! kinds and numbers are bare words; strings are quoted. Integers and reals must be canonical.
//! Every decoded preset is rebuilt through [`PdfPreset::from_values`], so every contract refusal
//! applies. Refusals are typed [`CodecError`]s naming a line number or a setting, never caller text.
#![deny(clippy::arithmetic_side_effects)]

use super::{
    key_position, preset_outcome_for, EnumValue, PdfPreset, PresetDraft, PresetError, PresetSource, QualityEntry, QualityValue, SettingKind,
    SettingRef, SettingSpec, SettingValue, SourceInput, MAX_DESCRIPTION_BYTES, MAX_FONT_LIST, MAX_LOCALES, MAX_QUALITY_ENTRIES,
    MAX_QUALITY_NUMBERS, SETTINGS,
};
use hsk_studio_observe::{FailureCode, Outcome};

/// The first line of every encoded preset.
pub const CODEC_HEADER: &str = "hsk.studio.pdf_preset@1";
/// Largest input accepted, in bytes.
pub const MAX_CODEC_BYTES: usize = 4 * 1024 * 1024;
/// Most lines (including comments and blank lines).
pub const MAX_CODEC_LINES: usize = 1024;
/// Longest line in bytes.
pub const MAX_CODEC_LINE_BYTES: usize = 256 * 1024;
/// Most tokens on one line.
pub const MAX_CODEC_TOKENS: usize = 2048;
/// Longest decoded token in bytes (the longest value the contract admits, a description).
pub const MAX_CODEC_VALUE_BYTES: usize = MAX_DESCRIPTION_BYTES;

/// Typed codec refusal; `line` is 1-based.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodecError {
    TooLarge,
    NotUtf8,
    TooManyLines,
    LineTooLong { line: usize },
    TooManyTokens { line: usize },
    BadHeader,
    Syntax { line: usize },
    UnknownDirective { line: usize },
    UnknownKey { line: usize },
    DuplicateKey { line: usize },
    DuplicateField { line: usize },
    ValueTooLong { line: usize },
    /// A head field (`id`, `name`, `source`, `default_locale`) or the `end` line is missing.
    MissingField { field: &'static str },
    MissingSetting(SettingRef),
    /// The decoded values were refused by the preset contract.
    Preset(PresetError),
}
impl CodecError {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::TooLarge => "codec_too_large",
            Self::NotUtf8 => "codec_not_utf8",
            Self::TooManyLines => "codec_too_many_lines",
            Self::LineTooLong { .. } => "codec_line_too_long",
            Self::TooManyTokens { .. } => "codec_too_many_tokens",
            Self::BadHeader => "codec_bad_header",
            Self::Syntax { .. } => "codec_syntax",
            Self::UnknownDirective { .. } => "codec_unknown_directive",
            Self::UnknownKey { .. } => "codec_unknown_key",
            Self::DuplicateKey { .. } => "codec_duplicate_key",
            Self::DuplicateField { .. } => "codec_duplicate_field",
            Self::ValueTooLong { .. } => "codec_value_too_long",
            Self::MissingField { .. } => "codec_missing_field",
            Self::MissingSetting(_) => "codec_missing_setting",
            Self::Preset(e) => e.code(),
        }
    }
}
impl std::fmt::Display for CodecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LineTooLong { line }
            | Self::TooManyTokens { line }
            | Self::Syntax { line }
            | Self::UnknownDirective { line }
            | Self::UnknownKey { line }
            | Self::DuplicateKey { line }
            | Self::DuplicateField { line }
            | Self::ValueTooLong { line } => write!(f, "{}: line {line}", self.code()),
            Self::MissingField { field } => write!(f, "{}: {field}", self.code()),
            Self::MissingSetting(r) => write!(f, "{}: {r}", self.code()),
            Self::Preset(e) => write!(f, "{e}"),
            _ => f.write_str(self.code()),
        }
    }
}
impl std::error::Error for CodecError {}

/// Maps a codec result onto the Observe outcome vocabulary (codes only); contract refusals map as
/// [`preset_outcome_for`], every syntax refusal is `Validation`.
pub fn codec_outcome_for<T>(result: &Result<T, CodecError>) -> Outcome {
    match result {
        Ok(_) => Outcome::Success,
        Err(CodecError::Preset(e)) => preset_outcome_for::<()>(&Err(*e)),
        Err(_) => Outcome::Failure(FailureCode::Validation),
    }
}

fn quote(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\u{{{:x}}}", u32::from(c))),
            c => out.push(c),
        }
    }
    out.push('"');
}

fn push_value(out: &mut String, value: &SettingValue) {
    match value {
        SettingValue::Bool(b) => out.push_str(&format!("bool {b}")),
        SettingValue::Integer(n) => out.push_str(&format!("int {n}")),
        SettingValue::Real(x) => out.push_str(&format!("real {x:?}")),
        SettingValue::Enum(e) => {
            out.push_str("enum ");
            quote(out, e.token());
        }
        SettingValue::FontList(l) => {
            out.push_str(&format!("fonts {}", l.len()));
            for n in l {
                out.push(' ');
                quote(out, n);
            }
        }
        SettingValue::Quality(q) => {
            out.push_str(&format!("quality {}", q.len()));
            for e in q {
                out.push(' ');
                quote(out, &e.key);
                match &e.value {
                    QualityValue::Number(x) => out.push_str(&format!(" num {x:?}")),
                    QualityValue::Integer(n) => out.push_str(&format!(" int {n}")),
                    QualityValue::Bool(b) => out.push_str(&format!(" bool {b}")),
                    QualityValue::Name(n) => {
                        out.push_str(" name ");
                        quote(out, n);
                    }
                    QualityValue::Numbers(xs) => {
                        out.push_str(&format!(" nums {}", xs.len()));
                        for x in xs {
                            out.push_str(&format!(" {x:?}"));
                        }
                    }
                }
            }
        }
        SettingValue::Text(t) => {
            out.push_str("text ");
            quote(out, t);
        }
    }
}

/// Encodes `preset` canonically: the same preset always yields the same bytes.
pub fn encode(preset: &PdfPreset) -> String {
    let mut out = String::new();
    out.push_str(CODEC_HEADER);
    out.push_str("\nid ");
    quote(&mut out, preset.id());
    out.push_str("\nname ");
    quote(&mut out, preset.name());
    out.push_str("\nsource ");
    match preset.source() {
        PresetSource::Shipped => out.push_str("shipped"),
        PresetSource::Operator { parent: None } => out.push_str("operator"),
        PresetSource::Operator { parent: Some(p) } => {
            out.push_str("operator ");
            quote(&mut out, p);
        }
        PresetSource::Imported { origin } => {
            out.push_str("imported ");
            quote(&mut out, origin);
        }
    }
    out.push_str("\ndefault_locale ");
    quote(&mut out, preset.descriptions().default_locale());
    for (locale, text) in preset.descriptions().entries() {
        out.push_str("\ndescription ");
        quote(&mut out, locale);
        out.push(' ');
        quote(&mut out, text);
    }
    for (spec, value) in preset.settings() {
        out.push_str("\nsetting ");
        out.push_str(&spec.setting().key());
        out.push(' ');
        push_value(&mut out, value);
    }
    out.push_str("\nend\n");
    out
}

#[derive(Debug, PartialEq)]
enum Tok<'a> {
    Bare(&'a str),
    Quoted(String),
}

const BLANK: [char; 3] = [' ', '\t', '\r'];

/// Decodes one quoted string after its opening quote; returns the text and the rest of the line.
fn quoted(mut rest: &str, line: usize) -> Result<(String, &str), CodecError> {
    let syntax = CodecError::Syntax { line };
    let mut out = String::new();
    loop {
        let mut it = rest.chars();
        let c = it.next().ok_or(syntax)?;
        rest = it.as_str();
        match c {
            '"' => return Ok((out, rest)),
            '\\' => {
                let mut it = rest.chars();
                let e = it.next().ok_or(syntax)?;
                rest = it.as_str();
                let decoded = match e {
                    '\\' => '\\',
                    '"' => '"',
                    'n' => '\n',
                    'r' => '\r',
                    't' => '\t',
                    'u' => {
                        let body = rest.strip_prefix('{').ok_or(syntax)?;
                        let close = body.find('}').ok_or(syntax)?;
                        let (hex, after) = body.split_at(close);
                        if hex.is_empty() || hex.len() > 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
                            return Err(syntax);
                        }
                        rest = after.strip_prefix('}').ok_or(syntax)?;
                        let v = u32::from_str_radix(hex, 16).map_err(|_| syntax)?;
                        char::from_u32(v).ok_or(syntax)?
                    }
                    _ => return Err(syntax),
                };
                out.push(decoded);
            }
            c if c.is_control() => return Err(syntax),
            c => out.push(c),
        }
        if out.len() > MAX_CODEC_VALUE_BYTES {
            return Err(CodecError::ValueTooLong { line });
        }
    }
}

fn tokenize(raw: &str, line: usize) -> Result<Vec<Tok<'_>>, CodecError> {
    let mut toks = Vec::new();
    let mut rest = raw;
    loop {
        rest = rest.trim_start_matches(BLANK);
        if rest.is_empty() {
            return Ok(toks);
        }
        if toks.len() >= MAX_CODEC_TOKENS {
            return Err(CodecError::TooManyTokens { line });
        }
        if let Some(after) = rest.strip_prefix('"') {
            let (text, tail) = quoted(after, line)?;
            if !(tail.is_empty() || tail.starts_with(BLANK)) {
                return Err(CodecError::Syntax { line });
            }
            toks.push(Tok::Quoted(text));
            rest = tail;
        } else {
            let end = rest.find(BLANK).unwrap_or(rest.len());
            let (word, tail) = rest.split_at(end);
            if word.len() > MAX_CODEC_VALUE_BYTES {
                return Err(CodecError::ValueTooLong { line });
            }
            if word.contains('"') {
                return Err(CodecError::Syntax { line });
            }
            toks.push(Tok::Bare(word));
            rest = tail;
        }
    }
}

/// A canonical decimal integer (no sign on zero, no `+`, no leading zero).
fn int_of(tok: Option<&Tok<'_>>, line: usize) -> Result<i64, CodecError> {
    match tok {
        Some(Tok::Bare(w)) => w.parse::<i64>().ok().filter(|n| n.to_string() == *w).ok_or(CodecError::Syntax { line }),
        _ => Err(CodecError::Syntax { line }),
    }
}
fn count_of(tok: Option<&Tok<'_>>, line: usize) -> Result<usize, CodecError> {
    let n = int_of(tok, line)?;
    usize::try_from(n).map_err(|_| CodecError::Syntax { line })
}
/// A real in Rust's shortest round-trip form (`{:?}`).
fn real_of(tok: Option<&Tok<'_>>, line: usize) -> Result<f64, CodecError> {
    match tok {
        Some(Tok::Bare(w)) => w.parse::<f64>().ok().filter(|x| format!("{x:?}") == *w).ok_or(CodecError::Syntax { line }),
        _ => Err(CodecError::Syntax { line }),
    }
}
fn bool_of(tok: Option<&Tok<'_>>, line: usize) -> Result<bool, CodecError> {
    match tok {
        Some(Tok::Bare("true")) => Ok(true),
        Some(Tok::Bare("false")) => Ok(false),
        _ => Err(CodecError::Syntax { line }),
    }
}
fn string_of(tok: Option<&Tok<'_>>, line: usize) -> Result<String, CodecError> {
    match tok {
        Some(Tok::Quoted(s)) => Ok(s.clone()),
        _ => Err(CodecError::Syntax { line }),
    }
}

/// Parses the value tokens of one setting line (after the key).
fn setting_value(spec: &SettingSpec, toks: &[Tok<'_>], line: usize) -> Result<SettingValue, CodecError> {
    let r = spec.setting();
    let syntax = CodecError::Syntax { line };
    let Some(Tok::Bare(kind)) = toks.first() else {
        return Err(syntax);
    };
    let args = toks.get(1..).unwrap_or(&[]);
    let exact = |n: usize| if args.len() == n { Ok(()) } else { Err(syntax) };
    let wrong = CodecError::Preset(PresetError::WrongValueKind(r));
    let value = match (spec.kind, *kind) {
        (SettingKind::Bool, "bool") => {
            exact(1)?;
            SettingValue::Bool(bool_of(args.first(), line)?)
        }
        (SettingKind::Integer(_), "int") => {
            exact(1)?;
            SettingValue::Integer(int_of(args.first(), line)?)
        }
        (SettingKind::Real(_), "real") => {
            exact(1)?;
            SettingValue::Real(real_of(args.first(), line)?)
        }
        (SettingKind::Enum(d), "enum") => {
            exact(1)?;
            let token = string_of(args.first(), line)?;
            let v: EnumValue = d.parse(&token).ok_or(CodecError::Preset(PresetError::UnknownMember(r)))?;
            SettingValue::Enum(v)
        }
        (SettingKind::Text, "text") => {
            exact(1)?;
            SettingValue::Text(string_of(args.first(), line)?)
        }
        (SettingKind::FontList, "fonts") => {
            let n = count_of(args.first(), line)?;
            if n > MAX_FONT_LIST {
                return Err(CodecError::Preset(PresetError::ListTooLong(r)));
            }
            exact(n.checked_add(1).ok_or(syntax)?)?;
            let names = args.get(1..).unwrap_or(&[]).iter().map(|t| string_of(Some(t), line)).collect::<Result<Vec<_>, _>>()?;
            SettingValue::FontList(names)
        }
        (SettingKind::QualityDict, "quality") => SettingValue::Quality(quality(r, args, line)?),
        _ => return Err(wrong),
    };
    Ok(value)
}

fn quality(r: SettingRef, args: &[Tok<'_>], line: usize) -> Result<Vec<QualityEntry>, CodecError> {
    let syntax = CodecError::Syntax { line };
    let n = count_of(args.first(), line)?;
    if n > MAX_QUALITY_ENTRIES {
        return Err(CodecError::Preset(PresetError::ListTooLong(r)));
    }
    let mut entries = Vec::with_capacity(n);
    let mut rest = args.get(1..).unwrap_or(&[]);
    while !rest.is_empty() {
        if entries.len() >= n {
            return Err(syntax);
        }
        let key = string_of(rest.first(), line)?;
        let kind = rest.get(1);
        let arg = rest.get(2);
        let (value, used) = match kind {
            Some(Tok::Bare("num")) => (QualityValue::Number(real_of(arg, line)?), 3),
            Some(Tok::Bare("int")) => (QualityValue::Integer(int_of(arg, line)?), 3),
            Some(Tok::Bare("bool")) => (QualityValue::Bool(bool_of(arg, line)?), 3),
            Some(Tok::Bare("name")) => (QualityValue::Name(string_of(arg, line)?), 3),
            Some(Tok::Bare("nums")) => {
                let k = count_of(arg, line)?;
                if k > MAX_QUALITY_NUMBERS {
                    return Err(CodecError::Preset(PresetError::QualityDictionaryInvalid(r)));
                }
                let used = k.checked_add(3).ok_or(syntax)?;
                let xs = rest.get(3..used).ok_or(syntax)?.iter().map(|t| real_of(Some(t), line)).collect::<Result<Vec<_>, _>>()?;
                (QualityValue::Numbers(xs), used)
            }
            _ => return Err(syntax),
        };
        entries.push(QualityEntry { key, value });
        rest = rest.get(used..).ok_or(syntax)?;
    }
    if entries.len() != n {
        return Err(syntax);
    }
    Ok(entries)
}

enum Source {
    Shipped,
    Operator(Option<String>),
    Imported(String),
}

fn set_once<T>(slot: &mut Option<T>, value: T, line: usize) -> Result<(), CodecError> {
    if slot.is_some() {
        return Err(CodecError::DuplicateField { line });
    }
    *slot = Some(value);
    Ok(())
}

/// Decodes `bytes` into a preset. Refuses typed (see [`CodecError`]); a decoded preset is rebuilt
/// through [`PdfPreset::from_values`], so the contract refusals apply as [`CodecError::Preset`].
pub fn decode(bytes: &[u8]) -> Result<PdfPreset, CodecError> {
    if bytes.len() > MAX_CODEC_BYTES {
        return Err(CodecError::TooLarge);
    }
    let text = std::str::from_utf8(bytes).map_err(|_| CodecError::NotUtf8)?;
    let mut id = None;
    let mut name = None;
    let mut source = None;
    let mut default_locale = None;
    let mut descriptions: Vec<(String, String)> = Vec::new();
    let mut values: Vec<(String, SettingValue)> = Vec::with_capacity(SETTINGS.len());
    let mut seen = vec![false; SETTINGS.len()];
    let mut ended = false;
    for (i, raw) in text.split('\n').enumerate() {
        if i >= MAX_CODEC_LINES {
            return Err(CodecError::TooManyLines);
        }
        let line = i.saturating_add(1);
        if raw.len() > MAX_CODEC_LINE_BYTES {
            return Err(CodecError::LineTooLong { line });
        }
        if i == 0 {
            if raw != CODEC_HEADER {
                return Err(CodecError::BadHeader);
            }
            continue;
        }
        let body = raw.trim_start_matches(BLANK);
        if body.is_empty() || body.starts_with('#') {
            continue;
        }
        if ended {
            return Err(CodecError::Syntax { line });
        }
        let toks = tokenize(raw, line)?;
        let Some(Tok::Bare(directive)) = toks.first() else {
            return Err(CodecError::Syntax { line });
        };
        let args = toks.get(1..).unwrap_or(&[]);
        let one = |n: usize| if args.len() == n { Ok(()) } else { Err(CodecError::Syntax { line }) };
        match *directive {
            "id" => {
                one(1)?;
                set_once(&mut id, string_of(args.first(), line)?, line)?;
            }
            "name" => {
                one(1)?;
                set_once(&mut name, string_of(args.first(), line)?, line)?;
            }
            "default_locale" => {
                one(1)?;
                set_once(&mut default_locale, string_of(args.first(), line)?, line)?;
            }
            "source" => {
                let s = match (args.first(), args.len()) {
                    (Some(Tok::Bare("shipped")), 1) => Source::Shipped,
                    (Some(Tok::Bare("operator")), 1) => Source::Operator(None),
                    (Some(Tok::Bare("operator")), 2) => Source::Operator(Some(string_of(args.get(1), line)?)),
                    (Some(Tok::Bare("imported")), 2) => Source::Imported(string_of(args.get(1), line)?),
                    _ => return Err(CodecError::Syntax { line }),
                };
                set_once(&mut source, s, line)?;
            }
            "description" => {
                one(2)?;
                if descriptions.len() >= MAX_LOCALES {
                    return Err(CodecError::Preset(PresetError::TooManyLocales));
                }
                descriptions.push((string_of(args.first(), line)?, string_of(args.get(1), line)?));
            }
            "setting" => {
                let Some(Tok::Bare(key)) = args.first() else {
                    return Err(CodecError::Syntax { line });
                };
                let idx = key_position(key).ok_or(CodecError::UnknownKey { line })?;
                let (Some(spec), Some(flag)) = (SETTINGS.get(idx), seen.get_mut(idx)) else {
                    return Err(CodecError::UnknownKey { line });
                };
                if *flag {
                    return Err(CodecError::DuplicateKey { line });
                }
                let value = setting_value(spec, args.get(1..).unwrap_or(&[]), line)?;
                *flag = true;
                values.push(((*key).to_owned(), value));
            }
            "end" => {
                one(0)?;
                ended = true;
            }
            _ => return Err(CodecError::UnknownDirective { line }),
        }
    }
    let missing = |field: &'static str| CodecError::MissingField { field };
    if !ended {
        return Err(missing("end"));
    }
    let id = id.ok_or(missing("id"))?;
    let name = name.ok_or(missing("name"))?;
    let source = source.ok_or(missing("source"))?;
    let default_locale = default_locale.ok_or(missing("default_locale"))?;
    if let Some((spec, _)) = SETTINGS.iter().zip(&seen).find(|(_, s)| !**s) {
        return Err(CodecError::MissingSetting(spec.setting()));
    }
    let descs: Vec<(&str, &str)> = descriptions.iter().map(|(l, t)| (l.as_str(), t.as_str())).collect();
    let source = match &source {
        Source::Shipped => SourceInput::Shipped,
        Source::Operator(p) => SourceInput::Operator { parent: p.as_deref() },
        Source::Imported(o) => SourceInput::Imported { origin: o.as_str() },
    };
    let head = PresetDraft { id: &id, name: &name, descriptions: &descs, default_locale: &default_locale, source, settings: &[] };
    PdfPreset::from_values(&head, &values).map_err(CodecError::Preset)
}
