use super::{xml, PaperEvidence, PublicationMeaning, ScientificEvidence};
use crate::{blogs, invalid, rss};
use rss_contracts::{Bounds, Error, Result};
use std::collections::BTreeMap;
use url::Url;
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    Daily {
        url: String,
    },
    Api {
        url: String,
        start: u64,
        page_size: usize,
    },
}
impl Source {
    pub fn url(&self) -> &str {
        match self {
            Self::Daily { url } | Self::Api { url, .. } => url,
        }
    }
}
pub fn source(raw: &str, bounds: &Bounds, max_entries: usize) -> Result<Source> {
    let url = blogs::public_url(raw, bounds)?;
    if url.scheme() != "https" || url.fragment().is_some() {
        return Err(invalid("science.source"));
    }
    if url.host_str() == Some("rss.arxiv.org") {
        let path = url.path().trim_matches('/');
        let Some((kind, categories)) = path.split_once('/') else {
            return Err(invalid("science.daily_source"));
        };
        if !matches!(kind, "rss" | "atom")
            || categories.is_empty()
            || categories.len() > 1000
            || !categories
                .bytes()
                .all(|v| v.is_ascii_alphanumeric() || matches!(v, b'.' | b'-' | b'+'))
            || url.query().is_some()
        {
            return Err(invalid("science.daily_source"));
        }
        return Ok(Source::Daily {
            url: url.to_string(),
        });
    }
    if url.host_str() != Some("export.arxiv.org") || url.path() != "/api/query" {
        return Err(invalid("science.api_source"));
    }
    let mut params = BTreeMap::new();
    for (key, value) in url.query_pairs() {
        if !matches!(
            key.as_ref(),
            "search_query" | "id_list" | "start" | "max_results" | "sortBy" | "sortOrder"
        ) || params
            .insert(key.into_owned(), value.into_owned())
            .is_some()
        {
            return Err(invalid("science.api_query"));
        }
    }
    if !params
        .get("search_query")
        .is_some_and(|v| !v.trim().is_empty())
        && !params.get("id_list").is_some_and(|v| !v.is_empty())
    {
        return Err(invalid("science.api_query"));
    }
    if let Some(ids) = params.get("id_list") {
        for id in ids.split(',') {
            paper_id(id)?;
        }
    }
    if params.get("sortBy").is_some_and(|v| {
        !matches!(
            v.as_str(),
            "relevance" | "submittedDate" | "lastUpdatedDate"
        )
    }) || params
        .get("sortOrder")
        .is_some_and(|v| !matches!(v.as_str(), "ascending" | "descending"))
    {
        return Err(invalid("science.api_sort"));
    }
    let start = params
        .get("start")
        .map(|v| v.parse::<u64>().map_err(|_| invalid("science.api_start")))
        .transpose()?
        .unwrap_or(0);
    let page_size = params
        .get("max_results")
        .map(|v| {
            v.parse::<usize>()
                .map_err(|_| invalid("science.api_page_size"))
        })
        .transpose()?
        .unwrap_or(100.min(max_entries));
    if start >= 30_000 || page_size == 0 || page_size > 2000 || page_size > max_entries {
        return Err(invalid("science.api_bounds"));
    }
    Ok(Source::Api {
        url: url.to_string(),
        start,
        page_size,
    })
}
pub fn request_url(source: &Source, start: u64) -> Result<String> {
    let Source::Api { url, page_size, .. } = source else {
        return Ok(source.url().into());
    };
    if start >= 30_000 {
        return Err(Error::LimitExceeded {
            field: "science.api_window".into(),
        });
    }
    let mut value = Url::parse(url).map_err(|_| invalid("science.api_url"))?;
    let params: Vec<_> = value
        .query_pairs()
        .filter(|(key, _)| key != "start" && key != "max_results")
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    value.set_query(None);
    for (key, param) in params {
        value.query_pairs_mut().append_pair(&key, &param);
    }
    value
        .query_pairs_mut()
        .append_pair("start", &start.to_string())
        .append_pair("max_results", &page_size.to_string());
    Ok(value.to_string())
}
pub fn paper_id(raw: &str) -> Result<(String, Option<u32>)> {
    let raw = raw
        .strip_prefix("oai:arXiv.org:")
        .or_else(|| raw.strip_prefix("arXiv:"))
        .unwrap_or(raw);
    let raw = if raw.starts_with("http://") || raw.starts_with("https://") {
        let url = Url::parse(raw).map_err(|_| invalid("science.paper_id"))?;
        if !matches!(url.host_str(), Some("arxiv.org" | "www.arxiv.org"))
            || !url.username().is_empty()
            || url.password().is_some()
            || url.port().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(invalid("science.paper_id"));
        }
        url.path()
            .strip_prefix("/abs/")
            .ok_or_else(|| invalid("science.paper_id"))?
            .to_owned()
    } else {
        raw.to_owned()
    };
    let (base, version) = if let Some((base, version)) = raw.rsplit_once('v') {
        let version = version
            .parse::<u32>()
            .ok()
            .filter(|v| *v > 0)
            .ok_or_else(|| invalid("science.paper_version"))?;
        (base, Some(version))
    } else {
        (raw.as_str(), None)
    };
    let digits = |value: &str| !value.is_empty() && value.bytes().all(|v| v.is_ascii_digit());
    let valid = if let Some((archive, number)) = base.split_once('/') {
        !archive.is_empty()
            && archive.len() <= 50
            && archive
                .bytes()
                .all(|v| v.is_ascii_alphabetic() || matches!(v, b'-' | b'.'))
            && number.len() == 7
            && digits(number)
            && number[2..4]
                .parse::<u8>()
                .is_ok_and(|m| (1..=12).contains(&m))
    } else if let Some((month, number)) = base.split_once('.') {
        month.len() == 4
            && digits(month)
            && month[2..4]
                .parse::<u8>()
                .is_ok_and(|m| (1..=12).contains(&m))
            && matches!(number.len(), 4 | 5)
            && digits(number)
    } else {
        false
    };
    if !valid {
        return Err(invalid("science.paper_id"));
    }
    Ok((base.into(), version))
}
pub struct Document {
    pub entries: Vec<ScientificEvidence>,
    pub digest: String,
    pub next: Option<u64>,
    pub total: Option<u64>,
    pub verified_empty: bool,
}
pub fn parse(
    source: &Source,
    start: u64,
    bytes: &[u8],
    max_bytes: usize,
    max_entries: usize,
    bounds: &Bounds,
) -> Result<Document> {
    let requested = request_url(source, start)?;
    let base = rss::parse(&requested, bytes, max_bytes, max_entries, bounds)?;
    let raw = xml::parse(bytes, max_entries, bounds)?;
    if raw.entries.len() != base.entries.len() {
        return Err(invalid("science.entry_alignment"));
    }
    if base.next_document.is_some() {
        return Err(invalid("science.unapproved_next"));
    }
    let mut entries = vec![];
    for (entry, raw) in base.entries.into_iter().zip(raw.entries) {
        if entry.reliable_provider_id.as_ref() != Some(&raw.id) {
            return Err(invalid("science.entry_alignment"));
        }
        let (id, version) = paper_id(&raw.id)?;
        let (link_id, link_version) = paper_id(&entry.url)?;
        if id != link_id || link_version.is_some_and(|v| Some(v) != version) {
            return Err(invalid("science.paper_link"));
        }
        let mut links = vec![];
        for link in raw.links {
            let link = blogs::public_url(&link, bounds)?.to_string();
            if !links.contains(&link) {
                links.push(link);
            }
        }
        let meaning = if matches!(source, Source::Api { .. }) {
            PublicationMeaning::FirstSubmission
        } else {
            PublicationMeaning::Announcement
        };
        let field = |name: &str| raw.fields.get(name).cloned();
        let paper = PaperEvidence {
            paper_id: id,
            version,
            original_entry_id: raw.id,
            publication_meaning: meaning,
            updated_literal: field("updated"),
            doi: field("doi").or_else(|| field("DOI")),
            journal_reference: field("journal_ref").or_else(|| field("journal_reference")),
            comment: field("comment"),
            announce_type: field("announce_type"),
            source_links: links,
        };
        let evidence = ScientificEvidence { entry, paper };
        if serde_json::to_vec(&evidence)
            .map_err(|_| invalid("science.evidence"))?
            .len()
            > bounds.max_operation_bytes
        {
            return Err(Error::LimitExceeded {
                field: "science.evidence".into(),
            });
        }
        entries.push(evidence);
    }
    let (next, total, verified_empty) = if let Source::Api { page_size, .. } = source {
        if raw.counters.len() != 3 {
            return Err(invalid("science.api_counters"));
        }
        let total = raw.counters["totalResults"];
        let returned_start = raw.counters["startIndex"];
        let size = raw.counters["itemsPerPage"];
        if returned_start != start
            || size > *page_size as u64
            || size < entries.len() as u64
            || entries.len() > *page_size
            || start > total && total != 0
        {
            return Err(invalid("science.api_counters"));
        }
        let end = start
            .checked_add(entries.len() as u64)
            .ok_or_else(|| invalid("science.api_offset"))?;
        if end > total {
            return Err(invalid("science.api_counters"));
        }
        if entries.is_empty() && total > start {
            return Err(invalid("science.api_unexpected_empty"));
        }
        let next = (end < total).then_some(end);
        if next.is_some_and(|offset| offset >= 30_000) {
            return Err(Error::LimitExceeded {
                field: "science.api_window".into(),
            });
        }
        (
            next,
            Some(total),
            entries.is_empty() && total == 0 && start == 0,
        )
    } else {
        (None, None, false)
    };
    Ok(Document {
        entries,
        digest: base.digest,
        next,
        total,
        verified_empty,
    })
}
