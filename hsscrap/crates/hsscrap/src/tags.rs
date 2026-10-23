//! Tag projections and intents. Hosts supply freshly authorized assignments and targets.
use std::collections::BTreeMap;
use crate::*;

/// Preserve each assignment's origin and target while grouping by canonical tag ID.
pub fn effective_tags<'a>(
    assignments: &'a [TagAssignment], scope: &ScopeRef, authorized_targets: &[TagTarget],
) -> BTreeMap<TagRef, Vec<&'a TagAssignment>> {
    let mut tags = BTreeMap::new();
    for assignment in assignments {
        if &assignment.scope == scope && assignment.removed_at.is_none()
            && authorized_targets.contains(&assignment.target)
        {
            tags.entry(assignment.tag_ref.clone()).or_insert_with(Vec::new).push(assignment);
        }
    }
    tags
}

/// The caller must resolve the tag through its canonical catalogue before this pure check.
pub fn add_assignment(
    candidate: TagAssignment, assignments: &[TagAssignment], scope: &ScopeRef,
    writable_targets: &[TagTarget],
) -> Result<TagAssignment> {
    if &candidate.scope != scope || !writable_targets.contains(&candidate.target) { return Err(Error::Denied); }
    if candidate.revision != Revision(1) || candidate.removed_at.is_some() {
        return Err(invalid("assignment", "new assignment must be active at revision 1"));
    }
    if let Some(existing) = assignments.iter().find(|a| a.assignment_id == candidate.assignment_id) {
        if existing == &candidate { return Ok(existing.clone()); }
        return Err(Error::Conflict { field: "assignment_id".into() });
    }
    for existing in assignments {
        if existing.scope == candidate.scope && existing.target == candidate.target
            && existing.tag_ref == candidate.tag_ref && existing.removed_at.is_none()
        { return Ok(existing.clone()); }
    }
    Ok(candidate)
}

pub fn remove_assignment(
    assignment: &TagAssignment, expected: Revision, at: Instant,
    scope: &ScopeRef, writable_targets: &[TagTarget],
) -> Result<TagAssignment> {
    if &assignment.scope != scope || !writable_targets.contains(&assignment.target) { return Err(Error::Denied); }
    if assignment.revision != expected { return Err(Error::Conflict { field: "assignment_revision".into() }); }
    if assignment.removed_at.is_some() { return Ok(assignment.clone()); }
    let mut removed = assignment.clone();
    removed.revision = expected.next()?;
    removed.removed_at = Some(at);
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn host(id: &str) -> HostRef { HostRef { host_instance_id: "host".into(), owner_kind: "loom".into(), native_id: id.into() } }
    fn assignment(id: u128, tag: &str, target: TagTarget) -> TagAssignment {
        TagAssignment { assignment_id: AssignmentId(uuid::Uuid::from_u128(id)), tag_ref: TagRef(host(tag)), target,
            scope: ScopeRef("private".into()), origin: TagOrigin::User, created_at: Instant::parse("2026-10-01T00:00:00Z").unwrap(),
            removed_at: None, revision: Revision(1), common: Extensions::default() }
    }
    #[test]
    fn inherited_removal_requires_owner_and_preserves_other_origins() {
        let item = TagTarget::Item(ItemId(uuid::Uuid::from_u128(1)));
        let project = TagTarget::Project(ProjectRef(host("project")));
        let assignments = vec![assignment(2, "same", item.clone()), assignment(3, "same", project.clone()), assignment(4, "distinct-id-same-label", item.clone())];
        let scope = ScopeRef("private".into());
        let tags = effective_tags(&assignments, &scope, &[item.clone(), project]);
        assert_eq!(tags.len(), 2);
        assert_eq!(tags[&TagRef(host("same"))].len(), 2);
        let at = Instant::parse("2026-10-02T00:00:00Z").unwrap();
        assert_eq!(remove_assignment(&assignments[1], Revision(1), at.clone(), &scope, &[item.clone()]), Err(Error::Denied));
        let mut next = assignments.clone();
        next[0] = remove_assignment(&assignments[0], Revision(1), at, &scope, &[item]).unwrap();
        assert_eq!(next[1], assignments[1]);
        assert_eq!(next[0].revision, Revision(2));
        assert!(effective_tags(&next, &ScopeRef("other".into()), &[assignments[1].target.clone()]).is_empty());
    }

    #[test]
    fn tag_identity_collision_is_checked_before_assignment_deduplication() {
        let item = TagTarget::Item(ItemId(uuid::Uuid::from_u128(1)));
        let assignments = vec![assignment(2, "same", item.clone()), assignment(3, "other", item.clone())];
        let candidate = assignment(3, "same", item.clone());
        assert!(matches!(add_assignment(candidate, &assignments, &ScopeRef("private".into()), &[item.clone()]), Err(Error::Conflict { .. })));
        let duplicate = assignment(4, "same", item.clone());
        assert_eq!(add_assignment(duplicate, &assignments, &ScopeRef("private".into()), &[item]).unwrap(), assignments[0]);
    }
}
