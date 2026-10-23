use crate::sources::SourceLibrary;
use rss_contracts::{Bounds, Error, HostRef, Result, Revision, ScopeRef};
use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberRule {
    pub keywords: Vec<String>,
    pub prioritize: bool,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuleLimits {
    pub max_keywords_per_source: usize,
    pub max_keyword_characters: usize,
    pub max_account_keywords: usize,
}
impl SourceLibrary {
    pub fn set_member_rule(&self, scope: &ScopeRef, expected: Revision, group: &HostRef,
        source: &HostRef, mut rule: MemberRule, bounds: &Bounds, limits: RuleLimits) -> Result<Self> {
        self.authorize(scope, expected)?;
        bounds.validate()?;
        if [limits.max_keywords_per_source, limits.max_keyword_characters, limits.max_account_keywords].contains(&0) {
            return Err(Error::Invalid { field: "rule_limits".into() });
        }
        let key = (group.clone(), source.clone());
        if !self.memberships.contains(&key) { return Err(Error::NotFound); }
        if self.groups.get(group).is_none_or(|g| &g.owner_scope != scope)
            || self.sources.get(source).is_none_or(|s| &s.owner_scope != scope) { return Err(Error::Denied); }
        if rule.keywords.len() > limits.max_keywords_per_source {
            return Err(Error::LimitExceeded { field: "rule.keywords".into() });
        }
        let mut unique = BTreeSet::new();
        for keyword in &mut rule.keywords {
            *keyword = keyword.trim().to_owned();
            if keyword.is_empty() || keyword.chars().any(char::is_control) || !unique.insert(keyword.to_lowercase()) {
                return Err(Error::Invalid { field: "rule.keywords".into() });
            }
            if keyword.chars().count() > limits.max_keyword_characters {
                return Err(Error::LimitExceeded { field: "rule.keyword_characters".into() });
            }
            bounds.text("rule.keywords", keyword, bounds.max_text_bytes)?;
        }
        if self.member_rules.get(&key) == Some(&rule) { return Ok(self.clone()); }
        let retained = self.member_rules.iter().filter(|(existing, _)| *existing != &key)
            .try_fold(0usize, |sum, (_, value)| sum.checked_add(value.keywords.len()))
            .ok_or_else(|| Error::LimitExceeded { field: "rule.account_keywords".into() })?;
        if retained.checked_add(rule.keywords.len()).is_none_or(|n| n > limits.max_account_keywords) {
            return Err(Error::LimitExceeded { field: "rule.account_keywords".into() });
        }
        let mut next = self.clone(); next.member_rules.insert(key, rule); next.advance()?; Ok(next)
    }
}
