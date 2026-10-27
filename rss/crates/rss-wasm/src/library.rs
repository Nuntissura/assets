//! Versioned snapshots convert to existing native library/query contracts.
use crate::boundary::*;

fn reference(value: &HostRef, maximum: usize) -> Result<(), String> {
    if [&value.host_instance_id, &value.owner_kind, &value.native_id]
        .iter()
        .any(|part| part.is_empty() || part.len() > maximum || part.chars().any(char::is_control))
    {
        return Err("invalid_reference".into());
    }
    Ok(())
}
use rss_contracts::{
    AcquisitionState, Bounds, Extensions, HostRef, Revision, ScopeRef, Source, TagRef,
};
use rss_core::query::*;
use rss_core::{MemberRule, ProfileSelection, SourceGroup, SourceLibrary};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Group {
    pub group_ref: HostRef,
    pub owner_scope: ScopeRef,
    pub name: String,
    pub description: String,
    pub keywords: Vec<String>,
    pub priority: i32,
    pub tag_refs: BTreeSet<TagRef>,
    pub revision: Revision,
    pub common: Extensions,
}
impl From<Group> for SourceGroup {
    fn from(group: Group) -> Self {
        Self {
            group_ref: group.group_ref,
            owner_scope: group.owner_scope,
            name: group.name,
            description: group.description,
            keywords: group.keywords,
            priority: group.priority,
            tag_refs: group.tag_refs,
            revision: group.revision,
            common: group.common,
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Membership {
    pub group: HostRef,
    pub source: HostRef,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    pub group: HostRef,
    pub source: HostRef,
    pub keywords: Vec<String>,
    pub prioritize: bool,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Selection {
    pub owner_scope: ScopeRef,
    pub profile_ref: HostRef,
    pub selected_groups: BTreeSet<HostRef>,
    pub selected_sources: BTreeSet<HostRef>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceHealth {
    pub source: HostRef,
    pub state: AcquisitionState,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub version: u32,
    pub scope: ScopeRef,
    pub library_revision: Revision,
    pub sources: Vec<Source>,
    pub groups: Vec<Group>,
    pub memberships: Vec<Membership>,
    pub rules: Vec<Rule>,
    pub selection: Selection,
    pub selection_revision: Revision,
    pub authority_epoch: String,
    pub authorized_sources: BTreeSet<HostRef>,
    pub access_partitions: BTreeSet<ScopeRef>,
    pub focused_group: Option<HostRef>,
    pub context: ContextFilter,
    pub filter: WallFilter,
    pub sort: WallSort,
    pub page_size: PageSize,
    pub health: Vec<SourceHealth>,
    pub bounds: Bounds,
}

pub fn compose(input: &str) -> Result<String, String> {
    let request: Request = decode(input)?;
    version(request.version)?;
    bounds(&request.bounds)?;
    for length in [
        request.sources.len(),
        request.groups.len(),
        request.memberships.len(),
        request.rules.len(),
        request.health.len(),
        request.selection.selected_groups.len(),
        request.selection.selected_sources.len(),
        request.authorized_sources.len(),
    ] {
        if length > request.bounds.max_collection_items {
            return Err("bounds".into());
        }
    }
    let mut library = SourceLibrary::empty(request.scope.clone());
    library.revision = request.library_revision;
    library
        .revision
        .validate()
        .map_err(|_| "invalid_revision")?;
    for source in request.sources {
        source.source_tags(&request.bounds).map_err(|_| "invalid_source_tags")?;
        reference(&source.source_ref, request.bounds.max_text_bytes)?;
        source
            .common
            .validate_bounded(&request.bounds, &BTreeSet::new())
            .map_err(|_| "unsupported_source")?;
        source.revision.validate().map_err(|_| "invalid_revision")?;
        for (text, maximum) in [
            (&source.display_name, request.bounds.max_text_bytes),
            (&source.original_url, request.bounds.max_url_bytes),
        ] {
            if text.len() > maximum {
                return Err("bounds".into());
            }
        }
        if source.owner_scope != request.scope
            || library
                .sources
                .insert(source.source_ref.clone(), source)
                .is_some()
        {
            return Err("invalid_source".into());
        }
    }
    for group in request.groups {
        reference(&group.group_ref, request.bounds.max_text_bytes)?;
        group
            .common
            .validate_bounded(&request.bounds, &BTreeSet::new())
            .map_err(|_| "unsupported_group")?;
        group.revision.validate().map_err(|_| "invalid_revision")?;
        if group.keywords.len() > request.bounds.max_collection_items
            || group.tag_refs.len() > request.bounds.max_collection_items
        {
            return Err("bounds".into());
        }
        for value in std::iter::once(&group.name)
            .chain(std::iter::once(&group.description))
            .chain(group.keywords.iter())
        {
            if value.len() > request.bounds.max_text_bytes {
                return Err("bounds".into());
            }
        }
        if group.owner_scope != request.scope
            || library
                .groups
                .insert(group.group_ref.clone(), group.into())
                .is_some()
        {
            return Err("invalid_group".into());
        }
    }
    for membership in request.memberships {
        if !library.groups.contains_key(&membership.group)
            || !library.sources.contains_key(&membership.source)
            || !library
                .memberships
                .insert((membership.group, membership.source))
        {
            return Err("invalid_membership".into());
        }
    }
    for rule in request.rules {
        for keyword in &rule.keywords {
            request
                .bounds
                .text("rule.keyword", keyword, request.bounds.max_text_bytes)
                .map_err(|_| "bounds")?;
        }
        let key = (rule.group, rule.source);
        if !library.memberships.contains(&key)
            || rule.keywords.len() > request.bounds.max_collection_items
            || library.member_rules.contains_key(&key)
        {
            return Err("invalid_rule".into());
        }
        library.member_rules.insert(
            key,
            MemberRule {
                keywords: rule.keywords,
                prioritize: rule.prioritize,
            },
        );
    }
    let selection = ProfileSelection {
        owner_scope: request.selection.owner_scope,
        profile_ref: request.selection.profile_ref,
        selected_groups: request.selection.selected_groups,
        selected_sources: request.selection.selected_sources,
    };
    let query = WallQuery::from_library(
        &library,
        &selection,
        request.selection_revision,
        request.authority_epoch,
        &request.authorized_sources,
        request.access_partitions,
        request.focused_group.as_ref(),
        request.context,
        request.filter,
        request.sort,
        request.page_size,
        &request.bounds,
    )
    .map_err(|_| "invalid_selection")?;
    bound(&query, MAX_QUERY)?;
    let mut states = BTreeMap::new();
    for health in request.health {
        let texts: Vec<&str> = match &health.state {
            AcquisitionState::Ready { last_success_at } => vec![last_success_at.as_str()],
            AcquisitionState::VerifiedEmpty {
                observed_at,
                selected_source_identity,
            } => vec![observed_at.as_str(), selected_source_identity],
            AcquisitionState::Partial {
                observed_at,
                reason_code,
            } => vec![observed_at.as_str(), reason_code],
            AcquisitionState::Failed { reason_code } => vec![reason_code],
            _ => vec![],
        };
        for text in texts {
            request
                .bounds
                .text("source.health", text, request.bounds.max_text_bytes)
                .map_err(|_| "bounds")?;
        }
        if !library.sources.contains_key(&health.source)
            || states.insert(health.source, health.state).is_some()
        {
            return Err("invalid_health".into());
        }
    }
    let model = ManagerModel::from_library(
        &library,
        &selection,
        &request.authorized_sources,
        &states,
        &request.bounds,
    )
    .map_err(|_| "invalid_selection")?;
    let sources: Vec<_> = model
        .sources
        .iter()
        .map(|source| {
            json!({"source":source.source,"selected":source.selected,
        "visible":source.visible,"groups":source.groups,"state":source.state})
        })
        .collect();
    let groups: Vec<_> = model
        .groups
        .iter()
        .map(|group| {
            json!({"group_ref":group.group.group_ref,"name":group.group.name,
        "selected":group.selected})
        })
        .collect();
    encode(&json!({"version":1,"query":query,"manager":{"sources":sources,"groups":groups}}))
}
