use serde::{Deserialize, Serialize};

/// URL-entry recognition is independent from provider support and permission to save.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceForm {
    Prefix,
    Url,
}

/// Presentation and entry-action availability for the current literal query.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "intent", rename_all = "snake_case", deny_unknown_fields)]
pub enum InputState {
    Neutral,
    Source {
        candidate: String,
        form: SourceForm,
        can_add_source: bool,
    },
    Quicknote,
}

pub(crate) fn classify(text: &str, composing: bool) -> InputState {
    if text.trim().is_empty() {
        return InputState::Neutral;
    }
    let candidate = text.strip_prefix(' ').unwrap_or(text);
    let candidate = candidate.strip_suffix(' ').unwrap_or(candidate);
    // Check before parsing: URL parsers may discard whitespace that is literal prose here.
    if candidate
        .chars()
        .any(|ch| ch.is_whitespace() || ch.is_control())
    {
        return InputState::Quicknote;
    }
    let form = if ["https://www.", "http://www.", "www."]
        .iter()
        .any(|prefix| candidate.eq_ignore_ascii_case(prefix))
    {
        Some(SourceForm::Prefix)
    } else if url::Url::parse(candidate).is_ok_and(|value| value.has_host()) {
        Some(SourceForm::Url)
    } else if !candidate.contains("://")
        && url::Url::parse(&format!("https://{candidate}")).is_ok_and(|value| {
            value.username().is_empty()
                && value.password().is_none()
                && value.domain().is_some_and(|domain| {
                    domain.rsplit_once('.').is_some_and(|(name, suffix)| {
                        !name.is_empty()
                            && !suffix.is_empty()
                            && suffix.bytes().any(|ch| ch.is_ascii_alphabetic())
                    })
                })
        })
    {
        Some(SourceForm::Url)
    } else {
        None
    };
    match form {
        Some(form) => InputState::Source {
            candidate: candidate.into(),
            form,
            can_add_source: !composing,
        },
        None => InputState::Quicknote,
    }
}

pub(crate) fn owner_entry(candidate: &str, form: SourceForm) -> Option<String> {
    if form == SourceForm::Prefix {
        return Some(if candidate.eq_ignore_ascii_case("www.") {
            "https://www.".into()
        } else {
            candidate.to_ascii_lowercase()
        });
    }
    url::Url::parse(candidate)
        .ok()
        .filter(|value| value.has_host())
        .or_else(|| url::Url::parse(&format!("https://{candidate}")).ok())
        .map(|value| value.to_string())
}
