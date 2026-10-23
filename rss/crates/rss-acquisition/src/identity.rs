use hsscrap::{identity::normalize_url, ports::{CommandContext, IdentityQuery, ItemOwner, UrlIdentityQuery}, ItemId};
use rss_contracts::{Error, Result};

/// Resolve before allocating an item or observation stream. Provider identifiers remain opaque strings.
pub fn resolve_capture_identity(owner: &impl ItemOwner, context: &CommandContext, original_url: &str,
    provider: Option<&IdentityQuery>) -> Result<Option<ItemId>> {
    crate::capture::validate_context(context, &context.scope)?;
    let normalized = normalize_url(original_url, &context.bounds)?;
    let mut matches = owner.lookup_url(context, UrlIdentityQuery { identity_url: normalized.identity_url,
        normalization_version: normalized.normalization_version, access_partition: context.scope.clone() })?;
    if matches.len() > context.bounds.max_collection_items { return Err(Error::LimitExceeded { field: "identity.url_matches".into() }); }
    let (provider_name, namespace, key) = if let Some(query) = provider {
        if query.access_partition != context.scope { return Err(Error::Denied); }
        for value in [&query.provider, &query.namespace, &query.provider_item_id] {
            if value.is_empty() { return Err(crate::invalid("identity.provider")); }
            context.bounds.text("identity.provider", value, context.bounds.max_text_bytes)?;
        }
        if let Some(item) = owner.lookup_identity(context, query.clone())? { matches.push(item); }
        (query.provider.as_str(), query.namespace.as_str(), query.provider_item_id.as_str())
    } else { ("", "", "") };
    Ok(hsscrap::identity::resolve_identity(&context.scope, provider_name, namespace, key, &[], &matches)?)
}
