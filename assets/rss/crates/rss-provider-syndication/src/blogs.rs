use std::collections::BTreeSet;
use rss_contracts::{Bounds, Result};
use scraper::{Html, Selector};
use url::Url;
use crate::invalid;

/// Syntactic public-web target only. The host separately authorizes DNS answers, address ranges and redirects.
pub fn public_url(value: &str, bounds: &Bounds) -> Result<Url> {
    bounds.text("url", value, bounds.max_url_bytes)?;
    let url = Url::parse(value).map_err(|_| invalid("url"))?;
    if !matches!(url.scheme(), "https" | "http") || url.host_str().is_none()
        || !url.username().is_empty() || url.password().is_some() { return Err(invalid("url")); }
    Ok(url)
}

/// Return advertised RSS/Atom candidates for explicit host selection; ambiguity never picks an arbitrary feed.
pub fn discover(base: &str, body: &[u8], max_bytes: usize, bounds: &Bounds) -> Result<Vec<String>> {
    if max_bytes == 0 || body.len() > max_bytes { return Err(rss_contracts::Error::LimitExceeded { field: "discovery.body".into() }); }
    let base = public_url(base, bounds)?;
    let html = std::str::from_utf8(body).map_err(|_| invalid("discovery.encoding"))?;
    let document = Html::parse_document(html);
    let selector = Selector::parse("link[rel][type][href]").map_err(|_| invalid("discovery.selector"))?;
    let mut feeds = BTreeSet::new();
    for element in document.select(&selector) {
        let element = element.value();
        if !element.attr("rel").unwrap_or("").split_ascii_whitespace().any(|value| value.eq_ignore_ascii_case("alternate")) { continue; }
        let media = element.attr("type").unwrap_or("").split(';').next().unwrap_or("").trim();
        if !media.eq_ignore_ascii_case("application/rss+xml") && !media.eq_ignore_ascii_case("application/atom+xml") { continue; }
        let url = base.join(element.attr("href").unwrap_or("")).map_err(|_| invalid("discovery.url"))?;
        let url = public_url(url.as_str(), bounds)?;
        feeds.insert(url.to_string());
        if feeds.len() > bounds.max_collection_items { return Err(rss_contracts::Error::LimitExceeded { field: "discovery.feeds".into() }); }
    }
    Ok(feeds.into_iter().collect())
}
