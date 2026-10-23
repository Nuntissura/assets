//! Capture URL syntax identity is separate from provider identity and retained-byte identity.
use std::collections::BTreeSet;
use crate::*;

pub const NORMALIZATION_VERSION: &str = "hsscrap.url.v1-url2.5.8-idna1.1.0";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedCaptureUrl {
    pub original: String,
    pub identity_url: String,
    pub normalization_version: String,
}

pub fn normalize_url(input: &str, bounds: &Bounds) -> Result<NormalizedCaptureUrl> {
    bounds.validate()?;
    bounds.text("url", input, bounds.max_url_bytes)?;
    if !crate::validation::http_url(input) || input.contains('\\') {
        return Err(invalid("url", "absolute http(s) URL without userinfo required"));
    }
    let parsed = url::Url::parse(input).map_err(|_| invalid("url", "invalid URL"))?;
    let (_, rest) = input.split_once("://").ok_or_else(|| invalid("url", "missing authority"))?;
    if rest.split(['/', '?', '#']).next().unwrap_or("").is_empty() {
        return Err(invalid("url", "empty authority cannot use parser recovery"));
    }
    let tail = rest.find(['/', '?', '#']).map(|i| &rest[i..]).unwrap_or("");
    let mut identity = format!("{}://{}", parsed.scheme(), parsed.host_str().ok_or_else(|| invalid("url", "missing host"))?);
    if let Some(port) = parsed.port() { identity.push_str(&format!(":{port}")); }
    if tail.is_empty() || tail.starts_with(['?', '#']) { identity.push('/'); }
    identity.push_str(tail);
    let mut bytes = identity.into_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if i + 2 >= bytes.len() || !bytes[i+1].is_ascii_hexdigit() || !bytes[i+2].is_ascii_hexdigit() {
                return Err(invalid("url", "invalid percent escape"));
            }
            bytes[i+1] = bytes[i+1].to_ascii_uppercase();
            bytes[i+2] = bytes[i+2].to_ascii_uppercase();
            i += 3;
        } else { i += 1; }
    }
    let identity_url = String::from_utf8(bytes).map_err(|_| invalid("url", "invalid UTF-8"))?;
    bounds.text("identity_url", &identity_url, bounds.max_url_bytes)?;
    Ok(NormalizedCaptureUrl { original: input.into(), identity_url, normalization_version: NORMALIZATION_VERSION.into() })
}

/// The owner supplies only currently authorized identities and URL matches from this scope.
pub fn resolve_identity(
    partition: &ScopeRef, provider: &str, namespace: &str, provider_item_id: &str,
    identities: &[SourceIdentity], authorized_url_matches: &[ItemId],
) -> Result<Option<ItemId>> {
    let mut candidates: BTreeSet<_> = authorized_url_matches.iter().copied().collect();
    for identity in identities {
        if &identity.access_partition == partition && identity.provider == provider && identity.namespace == namespace
            && identity.provider_item_id == provider_item_id && identity.retired_at.is_none() {
            candidates.insert(identity.item_id);
        }
    }
    if candidates.len() > 1 { return Err(Error::Conflict { field: "item_identity".into() }); }
    Ok(candidates.into_iter().next())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn bounds() -> Bounds { Bounds { max_url_bytes:4096,max_text_bytes:4096,max_collection_items:100,max_page_items:25,max_operation_bytes:20000 } }
    #[test] fn normalization_preserves_identity_significant_spelling() {
        let source="HTTPS://EXAMPLE.ORG:443/Case/%2f?b=2&a=1&a=3#Part";
        let value=normalize_url(source,&bounds()).unwrap();
        assert_eq!(value.original,source);
        assert_eq!(value.identity_url,"https://example.org/Case/%2F?b=2&a=1&a=3#Part");
        assert_eq!(normalize_url("http://EXAMPLE.ORG:80?x=1",&bounds()).unwrap().identity_url,"http://example.org/?x=1");
        assert_eq!(normalize_url("https://bücher.example/",&bounds()).unwrap().identity_url,"https://xn--bcher-kva.example/");
        assert_ne!(normalize_url("https://example.org/A",&bounds()).unwrap().identity_url,normalize_url("https://example.org/a",&bounds()).unwrap().identity_url);
        for bad in ["https://@example.org/","ftp://example.org/","https://example.org/%xz","https:///example.org/a"] { assert!(normalize_url(bad,&bounds()).is_err()); }
    }
    #[test] fn conflicting_matches_never_merge_and_partition_namespaces_are_scoped() {
        let first=ItemId(uuid::Uuid::from_u128(1));let other=ItemId(uuid::Uuid::from_u128(2));
        let identity=SourceIdentity { identity_id:IdentityId(uuid::Uuid::from_u128(3)), item_id:first,provider:"provider".into(),namespace:"posts/account-a".into(),provider_item_id:"42".into(),access_partition:ScopeRef("private".into()),verified_by_observation_id:ObservationId(uuid::Uuid::from_u128(4)),retired_at:None,common:Extensions::default() };
        assert_eq!(resolve_identity(&ScopeRef("private".into()),"provider","posts/account-a","42",&[identity.clone()],&[]).unwrap(),Some(first));
        assert!(matches!(resolve_identity(&ScopeRef("private".into()),"provider","posts/account-a","42",&[identity.clone()],&[other]),Err(Error::Conflict{..})));
        assert_eq!(resolve_identity(&ScopeRef("public".into()),"provider","posts/account-a","42",&[identity.clone()],&[]).unwrap(),None);
        assert_eq!(resolve_identity(&ScopeRef("private".into()),"provider","posts/account-b","42",&[identity],&[]).unwrap(),None);
    }
}
