//! Pure native completion plans. The Notes owner resolves ranges and performs transactions.
use crate::*;
use unicode_normalization::UnicodeNormalization;

pub const MAX_COMPLETION_SNAPSHOT_BYTES: usize = 65_536;
pub const MAX_COMPLETION_QUERY_BYTES: usize = 4_096;
pub const MAX_COMPLETION_TAGS: usize = 5_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NativeCompletionSelection {
    Caret,
    Text(String),
}

/// Only the bounded local suffix and selected text are copied, never a native document/tree.
/// The owner assigns a new ID when selection changes, even without a content revision change.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeCompletionSnapshot {
    fence: WritingFence,
    id: CompletionSnapshotId,
    before: String,
    selection: NativeCompletionSelection,
}
impl NativeCompletionSnapshot {
    pub fn new(fence: WritingFence, id: CompletionSnapshotId, before: String,
        selection: NativeCompletionSelection) -> Result<Self, Error> {
        let selected = match &selection { NativeCompletionSelection::Caret => 0,
            NativeCompletionSelection::Text(text) => text.len() };
        if before.len().checked_add(selected).is_none_or(|n| n > MAX_COMPLETION_SNAPSHOT_BYTES) {
            return Err(Error::TextLimit);
        }
        Ok(Self { fence, id, before, selection })
    }
    pub fn fence(&self) -> &WritingFence { &self.fence }
    pub fn id(&self) -> &CompletionSnapshotId { &self.id }
    pub fn text_before_caret(&self) -> &str { &self.before }
    pub fn selection(&self) -> &NativeCompletionSelection { &self.selection }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompletionInvocation { Automatic, Manual }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeCompletionKind { Manual, Operator, Note, Block, Tag, Template }

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NativeCompletionRange {
    /// The original owner selection, not an offset in the supplied text suffix.
    Selection,
    /// UTF-8 bytes in the ORIGINAL text_before_caret, not in normalized query text or Notes.
    BeforeCaret(ByteSpan),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NativeCompletionLookup {
    QueryFilter { prefix: String },
    Notes { title: String },
    Blocks { note_title: Option<String>, text: String, headings_only: bool },
    Tags { name: String },
    Templates { source: NativeTemplateSource },
}

/// Match a source row, then let Notes resolve its owning note for template insertion.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NativeTemplateSource {
    Note { title: String },
    Blocks { note_title: String, text: String, headings_only: bool },
}

/// Private construction retains the exact original snapshot independently of normalization.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeCompletionTrigger {
    snapshot: NativeCompletionSnapshot,
    kind: NativeCompletionKind,
    range: NativeCompletionRange,
    query: String,
    normalized: String,
    label: Option<String>,
    embed: bool,
}
impl NativeCompletionTrigger {
    pub fn snapshot(&self) -> &NativeCompletionSnapshot { &self.snapshot }
    pub fn kind(&self) -> NativeCompletionKind { self.kind }
    pub fn replacement_range(&self) -> &NativeCompletionRange { &self.range }
    pub fn query(&self) -> &str { &self.query }
    pub fn normalized_query(&self) -> &str { &self.normalized }
    pub fn label(&self) -> Option<&str> { self.label.as_deref() }
    pub fn embed(&self) -> bool { self.embed }
    pub fn composition_query(&self, provider: ProviderId, page_items: usize, cursor: Option<RevisionId>) -> CompositionQuery {
        CompositionQuery { fence: self.snapshot.fence.clone(), provider, query: self.normalized.clone(), page_items, cursor }
    }
    pub fn parsed_query(&self) -> Result<ParsedQuery, QueryError> {
        parse_query(&self.query.nfc().collect::<String>())
    }
    pub fn lookup(&self) -> NativeCompletionLookup {
        let query = &self.normalized;
        match self.kind {
            NativeCompletionKind::Operator => NativeCompletionLookup::QueryFilter { prefix: query.clone() },
            NativeCompletionKind::Tag => NativeCompletionLookup::Tags { name: query.clone() },
            NativeCompletionKind::Template => NativeCompletionLookup::Templates {
                source: if let Some((title, text, headings_only)) = reference_split(query) {
                    NativeTemplateSource::Blocks { note_title: title.to_owned(), text: text.to_owned(), headings_only }
                } else { NativeTemplateSource::Note { title: query.clone() } },
            },
            NativeCompletionKind::Manual | NativeCompletionKind::Note | NativeCompletionKind::Block => {
                if let Some((title, text, headings_only)) = reference_split(query) {
                    NativeCompletionLookup::Blocks {
                        note_title: (self.kind != NativeCompletionKind::Block).then(|| title.to_owned()),
                        text: text.to_owned(),
                        headings_only,
                    }
                } else if self.kind == NativeCompletionKind::Block {
                    NativeCompletionLookup::Blocks { note_title: None, text: query.clone(), headings_only: false }
                } else { NativeCompletionLookup::Notes { title: query.clone() } }
            }
        }
    }

    /// Supplied names must come from the host's current authorized tag set.
    pub fn operator_completions(&self, tags: &[&str]) -> Result<Vec<String>, Error> {
        validate_tags(tags)?;
        if self.kind != NativeCompletionKind::Operator { return Err(Error::InvalidSpan); }
        let values: Vec<String> = if self.normalized.starts_with("tag:") {
            tags.iter().map(|name| format!("tag:{}", if name.starts_with('#') || name.chars().any(|ch| separator(ch) || ch.is_control() || matches!(ch, '"' | '\\')) {
                quoted(name)
            } else { (*name).to_owned() })).collect()
        } else { QUERY_COMPLETIONS.iter().map(|text| (*text).to_owned()).collect() };
        Ok(values.into_iter().filter(|text| normalized(text).starts_with(&self.normalized))
            .map(|text| format!("{text} ")).collect())
    }

    /// A proposal only. Complete coverage is supplied by the canonical authorized owner.
    pub fn tag_creation_name(&self, tags: &[&str], coverage: CompletionTagCoverage) -> Result<Option<String>, Error> {
        validate_tags(tags)?;
        let name = self.query.trim_matches(separator);
        Ok((self.kind == NativeCompletionKind::Tag && coverage == CompletionTagCoverage::Complete
            && !name.is_empty() && name.len() <= 100
            && !tags.iter().any(|tag| normalized(tag) == normalized(name))).then(|| name.to_owned()))
    }
}

impl WritingProvider {
    /// Unlike prose search, explicit/marker completion may request a bounded empty-prefix page.
    /// This checks the declaration, not current database authorization or complete corpus coverage.
    pub fn validate_native_completion_query(&self, session: &WritingSession,
        trigger: &NativeCompletionTrigger, query: &CompositionQuery) -> Result<(), Error> {
        session.validate_native_completion(&trigger.snapshot.fence)?;
        if query.fence != trigger.snapshot.fence || query.provider != self.provider || query.query != trigger.normalized {
            return Err(Error::Stale);
        }
        if query.query.len() > self.max_query_bytes || query.page_items == 0 || query.page_items > self.max_page_items {
            return Err(Error::PayloadLimit);
        }
        if !self.capabilities.iter().any(|cap| cap.capability == WritingCapability::Search
            && matches!(cap.support, CapabilitySupport::Supported { .. })) {
            return Err(Error::Revoked);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompletionTagCoverage { Complete, Partial }

/// These descriptors are supplied only after authorized owner resolution. They grant no access.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NativeCompletionValue {
    Text { action: Action, text: String },
    Reference { target: NoteAddress, label: String, embed: bool },
    Tag { action: Action, label: String },
    Template,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeCompletionChoice { pub key: ResultKey, pub value: NativeCompletionValue }

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeCompletionPlan {
    snapshot: NativeCompletionSnapshot,
    range: NativeCompletionRange,
    choice: NativeCompletionChoice,
    action: WritingActionIntent,
}
impl NativeCompletionPlan {
    pub fn snapshot(&self) -> &NativeCompletionSnapshot { &self.snapshot }
    pub fn replacement_range(&self) -> &NativeCompletionRange { &self.range }
    pub fn choice(&self) -> &NativeCompletionChoice { &self.choice }
    pub fn action(&self) -> &WritingActionIntent { &self.action }
    /// Call again after asynchronous owner resolution and immediately before the native transaction.
    /// The owner must independently recheck authority and supply its CURRENT snapshot.
    pub fn validate(&self, session: &WritingSession, current: &NativeCompletionSnapshot) -> Result<(), Error> {
        session.validate_native_completion(&self.snapshot.fence)?;
        if &self.snapshot != current || session.selected() != Some(&self.choice.key) { return Err(Error::Stale); }
        if let NativeCompletionValue::Text { action, .. } | NativeCompletionValue::Tag { action, .. } = &self.choice.value {
            if !offered_completion_action(session, &self.choice.key, action) { return Err(Error::Stale); }
        }
        session.validate_action(&self.action)
    }
}

impl WritingSession {
    pub fn native_completion_trigger(&self, snapshot: &NativeCompletionSnapshot,
        invocation: CompletionInvocation) -> Result<Option<NativeCompletionTrigger>, Error> {
        self.validate_native_completion(&snapshot.fence)?;
        let (kind, range, raw, label, embed) = match &snapshot.selection {
            NativeCompletionSelection::Text(text) => {
                if invocation == CompletionInvocation::Automatic { return Ok(None); }
                (NativeCompletionKind::Manual, NativeCompletionRange::Selection, text.as_str(), None, false)
            }
            NativeCompletionSelection::Caret => {
                let text = &snapshot.before;
                let start = word_start(text);
                let word = &text[start..];
                let field = word.split_once(':').map(|(field, _)| field.to_ascii_lowercase());
                if field.as_deref().is_some_and(|field| ["type", "tag", "in", "is"].contains(&field)) {
                    (NativeCompletionKind::Operator, suffix(start, text.len()), word, None, false)
                } else if let Some(marker) = marker(text) { marker }
                else if invocation == CompletionInvocation::Manual {
                    (NativeCompletionKind::Manual, suffix(start, text.len()), word, None, false)
                } else { return Ok(None); }
            }
        };
        if raw.len() > MAX_COMPLETION_QUERY_BYTES { return Err(Error::TextLimit); }
        let normalized = normalized(raw);
        if normalized.len() > MAX_COMPLETION_QUERY_BYTES { return Err(Error::TextLimit); }
        Ok(Some(NativeCompletionTrigger { snapshot: snapshot.clone(), kind, range,
            query: raw.to_owned(), normalized, label, embed }))
    }

    pub fn plan_native_completion(&self, trigger: &NativeCompletionTrigger,
        current: &NativeCompletionSnapshot, mut choice: NativeCompletionChoice) -> Result<NativeCompletionPlan, Error> {
        self.validate_native_completion(&trigger.snapshot.fence)?;
        if &trigger.snapshot != current || self.selected() != Some(&choice.key) { return Err(Error::Stale); }
        let action = match (&mut choice.value, trigger.kind) {
            (NativeCompletionValue::Text { action, text }, NativeCompletionKind::Operator) => {
                if text.len() > MAX_COMPLETION_QUERY_BYTES || action.kind != ActionKind::ApplyCompletion {
                    return Err(Error::PayloadLimit);
                }
                if !offered_completion_action(self, &choice.key, action) { return Err(Error::Stale); }
                WritingAction::OwnerAction { action: action.clone() }
            }
            (NativeCompletionValue::Tag { action, label }, NativeCompletionKind::Tag) => {
                if label.len() > MAX_COMPLETION_QUERY_BYTES || action.kind != ActionKind::ApplyCompletion {
                    return Err(Error::PayloadLimit);
                }
                if !offered_completion_action(self, &choice.key, action) { return Err(Error::Stale); }
                WritingAction::OwnerAction { action: action.clone() }
            }
            (NativeCompletionValue::Template, NativeCompletionKind::Template) => WritingAction::Template { template: choice.key.clone() },
            (NativeCompletionValue::Reference { target, label, embed }, NativeCompletionKind::Manual | NativeCompletionKind::Note | NativeCompletionKind::Block) => {
                let compatible = matches!((trigger.lookup(), &*target),
                    (NativeCompletionLookup::Notes { .. }, NoteAddress::Note { .. })
                    | (NativeCompletionLookup::Blocks { headings_only: true, .. }, NoteAddress::Heading { .. })
                    | (NativeCompletionLookup::Blocks { headings_only: false, .. }, NoteAddress::Heading { .. } | NoteAddress::Block { .. }));
                if !compatible { return Err(Error::InvalidSpan); }
                if let Some(alias) = &trigger.label { if !alias.is_empty() { *label = alias.clone(); } }
                if label.len() > MAX_COMPLETION_QUERY_BYTES { return Err(Error::PayloadLimit); }
                *embed |= trigger.embed;
                WritingAction::Reference { target: target.clone(), embed: *embed }
            }
            _ => return Err(Error::InvalidSpan),
        };
        let action = WritingActionIntent { fence: trigger.snapshot.fence.clone(), action };
        self.validate_action(&action)?;
        Ok(NativeCompletionPlan { snapshot: trigger.snapshot.clone(), range: trigger.range.clone(), choice, action })
    }
}

fn offered_completion_action(session: &WritingSession, key: &ResultKey, action: &Action) -> bool {
    action.kind == ActionKind::ApplyCompletion && [Channel::Suggestions, Channel::Search].into_iter()
        .any(|channel| session.delivery(channel).items().iter().any(|item| &item.key == key && item.actions.contains(action)))
}
// Portal's /^(.*?)([#^])(.*)$/u has neither dotAll nor multiline enabled.
fn reference_split(query: &str) -> Option<(&str, &str, bool)> {
    if query.chars().any(|ch| matches!(ch, '\n' | '\r' | '\u{2028}' | '\u{2029}')) { return None; }
    let (at, marker) = query.char_indices().find(|(_, ch)| matches!(ch, '#' | '^'))?;
    let tail = &query[at + marker.len_utf8()..];
    Some((&query[..at], if tail.is_empty() { query } else { tail }, marker == '#'))
}
fn normalized(text: &str) -> String { text.nfc().collect::<String>().to_lowercase() }
fn suffix(start: usize, end: usize) -> NativeCompletionRange { NativeCompletionRange::BeforeCaret(ByteSpan { start, end }) }
// ECMAScript WhiteSpace/LineTerminator, matching the inspected Portal trigger contract.
fn separator(ch: char) -> bool {
    matches!(ch, '\u{0009}'..='\u{000d}' | ' ' | '\u{00a0}' | '\u{1680}' | '\u{2000}'..='\u{200a}'
        | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}' | '\u{feff}')
}
fn word_start(text: &str) -> usize {
    text.char_indices().rev().find(|(_, ch)| separator(*ch)).map_or(0, |(at, ch)| at + ch.len_utf8())
}
type Marker<'a> = (NativeCompletionKind, NativeCompletionRange, &'a str, Option<String>, bool);
fn marker(text: &str) -> Option<Marker<'_>> {
    let note_after = text.rfind([']', '\n']).map_or(0, |at| at + 1);
    let block_after = text.rfind([')', '\n']).map_or(0, |at| at + 1);
    let word = word_start(text);
    for (at, ch) in text.char_indices() {
        let tail = &text[at..];
        let (kind, prefix) = if at >= note_after && tail.starts_with("[[") {
            (NativeCompletionKind::Note, 2)
        } else if at >= block_after && tail.starts_with("((") {
            (NativeCompletionKind::Block, 2)
        } else if (ch == '#' || ch == '/') && at == word
            && !tail[1..].chars().any(|next| separator(next) || next == ch) {
            (if ch == '#' { NativeCompletionKind::Tag } else { NativeCompletionKind::Template }, 1)
        } else { continue };
        let raw = &tail[prefix..];
        let (query, label) = raw.split_once('|').map_or((raw, None), |(query, label)| (query, Some(label.to_owned())));
        let embed = matches!(kind, NativeCompletionKind::Note | NativeCompletionKind::Block)
            && text[..at].ends_with('!');
        return Some((kind, suffix(at - usize::from(embed), text.len()), query, label, embed));
    }
    None
}
fn validate_tags(tags: &[&str]) -> Result<(), Error> {
    if tags.len() > MAX_COMPLETION_TAGS || tags.iter().any(|tag| tag.trim_matches(separator).is_empty() || tag.len() > 100) {
        return Err(Error::PayloadLimit);
    }
    Ok(())
}
fn quoted(text: &str) -> String {
    let mut output = String::from("\"");
    for (index, ch) in text.chars().enumerate() {
        match ch {
            '#' if index == 0 => output.push_str("\\u0023"),
            '"' => output.push_str("\\\""), '\\' => output.push_str("\\\\"),
            '\n' => output.push_str("\\n"), '\r' => output.push_str("\\r"), '\t' => output.push_str("\\t"),
            '\u{0008}' => output.push_str("\\b"), '\u{000c}' => output.push_str("\\f"),
            ch if ch < '\u{0020}' => output.push_str(&format!("\\u{:04x}", u32::from(ch))),
            ch => output.push(ch),
        }
    }
    output.push('"'); output
}
const QUERY_COMPLETIONS: &[&str] = &[
    "type:note", "type:file", "type:folder", "type:command", "type:heading", "type:block", "type:task",
    "type:tag", "type:settings", "type:manual", "type:navigation", "type:feed", "in:title", "in:body",
    "is:todo", "is:doing", "is:done", "is:read", "is:unread", "is:bookmarked", "tag:",
];
