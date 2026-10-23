//! Pure organization plans. Hosts authorize resources, load canonical sets and commit atomically.
use crate::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedDestination {
    pub scope: ScopeRef,
    pub folder_ref: Option<FolderRef>,
    pub project_ref: Option<ProjectRef>,
}

#[cfg(test)]
mod tests {
    use super::*;
    fn host(id: &str) -> HostRef { HostRef { host_instance_id: "host".into(), owner_kind: "organization".into(), native_id: id.into() } }
    fn instant(day: u8) -> Instant { Instant::parse(&format!("2026-10-{day:02}T00:00:00Z")).unwrap() }
    fn membership(id: u128, project: &str) -> ProjectMembership {
        ProjectMembership { membership_id: MembershipId(uuid::Uuid::from_u128(id)), item_id: ItemId(uuid::Uuid::from_u128(1)),
            project_ref: ProjectRef(host(project)), scope: ScopeRef("private".into()), saved_at: instant(1),
            created_here_at: instant(1), revision: Revision(1), lifecycle: Lifecycle::Active, removed_at: None, common: Extensions::default() }
    }
    fn placement(id: u128, membership_id: MembershipId, folder: Option<&str>) -> FolderPlacement {
        FolderPlacement { placement_id: PlacementId(uuid::Uuid::from_u128(id)), item_id: ItemId(uuid::Uuid::from_u128(1)),
            membership_id: Some(membership_id), folder_ref: folder.map(|f| FolderRef(host(f))), scope: ScopeRef("private".into()),
            sort_order: None, placed_at: instant(2), moved_at: None, created_here_at: instant(2), revision: Revision(1),
            lifecycle: Lifecycle::Active, removed_at: None, common: Extensions::default() }
    }
    fn destination(project: &str, folder: Option<&str>) -> ResolvedDestination {
        ResolvedDestination { scope: ScopeRef("private".into()), project_ref: Some(ProjectRef(host(project))), folder_ref: folder.map(|f| FolderRef(host(f))) }
    }
    fn organization() -> Organization { Organization { scope: ScopeRef("private".into()), memberships: vec![], placements: vec![], canonical_set_complete: true } }

    #[test]
    fn saves_moves_and_explicit_readd_preserve_original_history() {
        let mut state = organization();
        let first = state.save_membership(membership(2, "a")).unwrap();
        let mut retry = membership(3, "a"); retry.saved_at = instant(3);
        assert_eq!(state.save_membership(retry).unwrap(), first);
        let root = state.add_placement(placement(4, first.membership_id, None), &destination("a", None)).unwrap();
        let moved = state.move_placement(root.placement_id, Revision(1), &destination("a", Some("folder")), Some(7), instant(3)).unwrap();
        assert_eq!(moved.placement_id, root.placement_id);
        assert_eq!(moved.placed_at, root.placed_at);
        assert_eq!(moved.moved_at, Some(instant(3)));
        assert_eq!(state.memberships[0].saved_at, instant(1));
        state.remove_placement(moved.placement_id, moved.revision, instant(4)).unwrap();
        assert_eq!(state.memberships[0].lifecycle, Lifecycle::Active);
        state.remove_membership_if_empty(first.membership_id, Revision(1), instant(4)).unwrap();
        let mut readd = membership(5, "a"); readd.saved_at = instant(5);
        state.save_membership(readd.clone()).unwrap();
        assert_eq!(state.memberships[0].lifecycle, Lifecycle::Tombstoned);
        assert_eq!(state.memberships[1].saved_at, instant(5));
        assert_ne!(state.memberships[0].membership_id, readd.membership_id);
    }

    #[test]
    fn cross_project_move_counts_every_placement_and_rejects_atomically() {
        let mut state = organization();
        let first = state.save_membership(membership(2, "a")).unwrap();
        let one = state.add_placement(placement(3, first.membership_id, Some("one")), &destination("a", Some("one"))).unwrap();
        state.add_placement(placement(4, first.membership_id, Some("two")), &destination("a", Some("two"))).unwrap();
        let before = state.clone();
        let next_member = membership(5, "b");
        let next_place = placement(6, next_member.membership_id, Some("dest"));
        assert!(matches!(state.cross_project_move(one.placement_id, one.revision, next_member.clone(), next_place.clone(),
            &destination("b", Some("dest")), SourceDisposition::RemoveIfEmpty, instant(5)), Err(Error::Conflict { .. })));
        assert_eq!(state, before);
        let kept = state.cross_project_move(one.placement_id, one.revision, next_member, next_place,
            &destination("b", Some("dest")), SourceDisposition::Keep, instant(5)).unwrap();
        assert_eq!(kept.memberships[0].lifecycle, Lifecycle::Active);
        assert_eq!(kept.placements[1], before.placements[1]);
        assert_eq!(kept.placements[0].lifecycle, Lifecycle::Tombstoned);
        let mut partial = state.clone(); partial.canonical_set_complete = false;
        assert!(partial.remove_membership_if_empty(first.membership_id, first.revision, instant(5)).is_err());
    }

    #[test]
    fn collision_and_cross_item_membership_do_not_change_state() {
        let mut state = organization();
        let member = state.save_membership(membership(2, "a")).unwrap();
        let one = state.add_placement(placement(3, member.membership_id, None), &destination("a", None)).unwrap();
        state.add_placement(placement(4, member.membership_id, Some("taken")), &destination("a", Some("taken"))).unwrap();
        let before = state.clone();
        assert!(matches!(state.move_placement(one.placement_id, Revision(1), &destination("a", Some("taken")), None, instant(3)), Err(Error::Conflict { .. })));
        assert_eq!(state, before);
        let mut wrong = placement(5, member.membership_id, Some("other")); wrong.item_id = ItemId(uuid::Uuid::from_u128(999));
        assert_eq!(state.add_placement(wrong, &destination("a", Some("other"))), Err(Error::ProjectContextUnresolved));
        assert_eq!(state, before);
    }

    #[test]
    fn cross_project_remove_if_empty_reuses_existing_destination_save_date() {
        let mut state = organization();
        let first = state.save_membership(membership(2, "a")).unwrap();
        let existing = state.save_membership(membership(3, "b")).unwrap();
        let source = state.add_placement(placement(4, first.membership_id, Some("from")), &destination("a", Some("from"))).unwrap();
        let mut retry = membership(5, "b"); retry.saved_at = instant(5);
        let plan = state.cross_project_move(source.placement_id, Revision(1), retry.clone(), placement(6, retry.membership_id, Some("to")),
            &destination("b", Some("to")), SourceDisposition::RemoveIfEmpty, instant(5)).unwrap();
        assert_eq!(plan.memberships.len(), 2);
        assert_eq!(plan.memberships[0].lifecycle, Lifecycle::Tombstoned);
        assert_eq!(plan.memberships[1], existing);
        assert_eq!(plan.placements[1].membership_id, Some(existing.membership_id));
        assert_eq!(plan.placements[1].placed_at, instant(2));
        assert_eq!(state.memberships[0].lifecycle, Lifecycle::Active);
    }

    #[test]
    fn occurrence_context_rejects_foreign_item_and_removed_membership() {
        let mut state = organization();
        let member = state.save_membership(membership(2, "a")).unwrap();
        let mut occurrence = OccurrenceReference {
            occurrence_ref: OccurrenceRef(host("note-embed")), item_id: member.item_id, member_id: None,
            context_membership_id: Some(member.membership_id), caption_override: Override::Inherit,
            presentation: None, revision: Revision(1), common: Extensions::default(),
        };
        state.validate_occurrence(&occurrence, &[]).unwrap();
        occurrence.item_id = ItemId(uuid::Uuid::from_u128(99));
        assert_eq!(state.validate_occurrence(&occurrence, &[]), Err(Error::ProjectContextUnresolved));
        occurrence.item_id = member.item_id;
        occurrence.member_id = Some(MemberId(uuid::Uuid::from_u128(10)));
        assert!(state.validate_occurrence(&occurrence, &[]).is_err());
        occurrence.member_id = None;
        state.remove_membership_if_empty(member.membership_id, member.revision, instant(5)).unwrap();
        assert_eq!(state.validate_occurrence(&occurrence, &[]), Err(Error::ProjectContextUnresolved));
        occurrence.context_membership_id = None;
        state.validate_occurrence(&occurrence, &[]).unwrap();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceDisposition { Keep, RemoveIfEmpty }

/// A host must load the entire membership/placement aggregate, not a UI page.
/// Commit these plans using the host owner's expected revision fence.
#[derive(Debug, Clone, PartialEq)]
pub struct Organization {
    pub scope: ScopeRef,
    pub memberships: Vec<ProjectMembership>,
    pub placements: Vec<FolderPlacement>,
    pub canonical_set_complete: bool,
}

impl Organization {
    fn complete(&self) -> Result<()> {
        if !self.canonical_set_complete { return Err(invalid("organization", "complete canonical set required")); }
        Ok(())
    }

    pub fn save_membership(&mut self, candidate: ProjectMembership) -> Result<ProjectMembership> {
        self.complete()?;
        if candidate.scope != self.scope { return Err(Error::Denied); }
        if candidate.lifecycle != Lifecycle::Active || candidate.removed_at.is_some() || candidate.revision != Revision(1) {
            return Err(invalid("membership", "new membership must be active at revision 1"));
        }
        if self.memberships.iter().any(|m| m.membership_id == candidate.membership_id && m != &candidate) {
            return Err(Error::Conflict { field: "membership_id".into() });
        }
        if let Some(existing) = self.memberships.iter().find(|m| m.scope == self.scope && m.item_id == candidate.item_id
            && m.project_ref == candidate.project_ref && m.lifecycle == Lifecycle::Active)
        { return Ok(existing.clone()); }
        self.memberships.push(candidate.clone());
        Ok(candidate)
    }

    fn validate_destination(&self, placement: &FolderPlacement, destination: &ResolvedDestination) -> Result<()> {
        if placement.scope != self.scope || destination.scope != self.scope { return Err(Error::Denied); }
        if placement.folder_ref != destination.folder_ref { return Err(Error::ProjectContextUnresolved); }
        match (&destination.project_ref, placement.membership_id) {
            (None, None) => Ok(()),
            (Some(project), Some(id)) => {
                let membership = self.memberships.iter().find(|m| m.membership_id == id)
                    .ok_or(Error::ProjectContextUnresolved)?;
                if membership.lifecycle != Lifecycle::Active || membership.item_id != placement.item_id
                    || membership.scope != self.scope || &membership.project_ref != project
                { return Err(Error::ProjectContextUnresolved); }
                Ok(())
            }
            _ => Err(Error::ProjectContextUnresolved),
        }
    }

    fn collision(&self, candidate: &FolderPlacement) -> Result<()> {
        if self.placements.iter().any(|p| p.placement_id != candidate.placement_id
            && p.lifecycle == Lifecycle::Active && p.scope == candidate.scope
            && p.folder_ref == candidate.folder_ref && p.item_id == candidate.item_id)
        { return Err(Error::Conflict { field: "placement_destination".into() }); }
        Ok(())
    }

    pub fn add_placement(&mut self, candidate: FolderPlacement, destination: &ResolvedDestination) -> Result<FolderPlacement> {
        self.complete()?;
        self.validate_destination(&candidate, destination)?;
        if candidate.lifecycle != Lifecycle::Active || candidate.removed_at.is_some() || candidate.revision != Revision(1) {
            return Err(invalid("placement", "new placement must be active at revision 1"));
        }
        if let Some(existing) = self.placements.iter().find(|p| p.placement_id == candidate.placement_id) {
            if existing == &candidate { return Ok(existing.clone()); }
            return Err(Error::Conflict { field: "placement_id".into() });
        }
        self.collision(&candidate)?;
        self.placements.push(candidate.clone());
        Ok(candidate)
    }

    /// Moving within a project keeps the original placement and save dates.
    pub fn move_placement(&mut self, id: PlacementId, expected: Revision, destination: &ResolvedDestination,
        sort_order: Option<i32>, at: Instant) -> Result<FolderPlacement>
    {
        self.complete()?;
        let index = self.placements.iter().position(|p| p.placement_id == id && p.lifecycle == Lifecycle::Active)
            .ok_or(Error::NotFound)?;
        let mut moved = self.placements[index].clone();
        if moved.revision != expected { return Err(Error::Conflict { field: "placement_revision".into() }); }
        moved.folder_ref = destination.folder_ref.clone();
        self.validate_destination(&moved, destination)?;
        self.collision(&moved)?;
        if moved.folder_ref == self.placements[index].folder_ref && moved.sort_order == sort_order { return Ok(moved); }
        moved.sort_order = sort_order;
        moved.moved_at = Some(at);
        moved.revision = expected.next()?;
        self.placements[index] = moved.clone();
        Ok(moved)
    }

    pub fn remove_placement(&mut self, id: PlacementId, expected: Revision, at: Instant) -> Result<FolderPlacement> {
        self.complete()?;
        let placement = self.placements.iter_mut().find(|p| p.placement_id == id).ok_or(Error::NotFound)?;
        if placement.scope != self.scope { return Err(Error::Denied); }
        if placement.revision != expected { return Err(Error::Conflict { field: "placement_revision".into() }); }
        if placement.lifecycle == Lifecycle::Active {
            placement.revision = expected.next()?;
            placement.lifecycle = Lifecycle::Tombstoned;
            placement.removed_at = Some(at);
        }
        Ok(placement.clone())
    }

    pub fn remove_membership_if_empty(&mut self, id: MembershipId, expected: Revision, at: Instant) -> Result<ProjectMembership> {
        self.complete()?;
        let membership = self.memberships.iter().find(|m| m.membership_id == id).ok_or(Error::NotFound)?;
        if membership.scope != self.scope { return Err(Error::Denied); }
        if membership.revision != expected { return Err(Error::Conflict { field: "membership_revision".into() }); }
        if self.placements.iter().any(|p| p.membership_id == Some(id) && p.lifecycle == Lifecycle::Active) {
            return Err(Error::Conflict { field: "membership_not_empty".into() });
        }
        let membership = self.memberships.iter_mut().find(|m| m.membership_id == id).ok_or(Error::NotFound)?;
        if membership.lifecycle == Lifecycle::Active {
            membership.revision = expected.next()?;
            membership.lifecycle = Lifecycle::Tombstoned;
            membership.removed_at = Some(at);
        }
        Ok(membership.clone())
    }

    /// Return a complete plan so a rejected cross-project move changes no state.
    pub fn cross_project_move(&self, source: PlacementId, expected: Revision,
        destination_membership: ProjectMembership, mut destination_placement: FolderPlacement,
        destination: &ResolvedDestination, disposition: SourceDisposition, at: Instant) -> Result<Self>
    {
        self.complete()?;
        let source_placement = self.placements.iter().find(|p| p.placement_id == source && p.lifecycle == Lifecycle::Active)
            .ok_or(Error::NotFound)?;
        if source_placement.item_id != destination_membership.item_id || source_placement.item_id != destination_placement.item_id {
            return Err(invalid("item_id", "cross-item membership or placement"));
        }
        if destination_placement.placement_id == source { return Err(Error::Conflict { field: "placement_id".into() }); }
        let source_membership = source_placement.membership_id.map(|id|
            self.memberships.iter().find(|m| m.membership_id == id && m.lifecycle == Lifecycle::Active)
                .ok_or(Error::ProjectContextUnresolved)).transpose()?;
        if source_membership.is_some_and(|m| m.project_ref == destination_membership.project_ref) {
            return Err(invalid("project_ref", "use within-project move"));
        }
        let mut plan = self.clone();
        let saved = plan.save_membership(destination_membership)?;
        destination_placement.membership_id = Some(saved.membership_id);
        plan.add_placement(destination_placement, destination)?;
        plan.remove_placement(source, expected, at.clone())?;
        if disposition == SourceDisposition::RemoveIfEmpty {
            if let Some(membership) = source_membership {
                plan.remove_membership_if_empty(membership.membership_id, membership.revision, at)?;
            }
        }
        Ok(plan)
    }

    pub fn validate_occurrence(&self, occurrence: &OccurrenceReference, members: &[MemberRef]) -> Result<()> {
        if let Some(member_id) = occurrence.member_id {
            if !members.iter().any(|m| m.item_id == occurrence.item_id && m.member_id == member_id) {
                return Err(invalid("member_id", "member does not belong to occurrence item"));
            }
        }
        if let Some(id) = occurrence.context_membership_id {
            if !self.memberships.iter().any(|m| m.membership_id == id && m.item_id == occurrence.item_id
                && m.scope == self.scope && m.lifecycle == Lifecycle::Active)
            { return Err(Error::ProjectContextUnresolved); }
        }
        Ok(())
    }
}
