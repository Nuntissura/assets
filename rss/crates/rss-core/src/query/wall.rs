use std::collections::{BTreeMap, BTreeSet};

use hsscrap::{
    interaction::Inspection, metadata::project_metadata, AvailabilityState, GalleryMember, Item,
    Lifecycle, MediaKind, MemberId, MemberLifecycle, MemberRef, MetadataObservation, Override,
    TagTarget, Validate,
};
use rss_contracts::{AcquisitionState, Bounds, Error, HostRef, ItemId, Result, Source};

use super::{invalid, QuerySession, SelectedContext, WallSnapshot};

/// Canonical owner projection; no copied tag/metadata editing semantics. The host
/// separately authorizes the gallery owner: GalleryMember has an observation ID,
/// but carries no access partition that this reducer can independently inspect.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct WallCard {
    pub item: Item,
    pub observation: Option<MetadataObservation>,
    pub members: Vec<GalleryMember>,
}
impl WallCard {
    pub fn validate(&self, bounds: &Bounds) -> Result<()> {
        let supported = BTreeSet::new();
        self.item.validate(bounds, &supported)?;
        if self.item.lifecycle != Lifecycle::Active {
            return Err(Error::Denied);
        }
        if self.members.len() > bounds.max_collection_items {
            return Err(invalid("card.members"));
        }
        project_metadata(&self.item, true, self.observation.as_ref())?;
        if let Some(observation) = &self.observation {
            observation.validate(bounds, &supported)?;
        }
        let mut active = BTreeSet::new();
        let mut all = BTreeSet::new();
        for member in &self.members {
            member.validate(bounds, &supported)?;
            if member.item_id != self.item.item_id || !all.insert(member.member_id) {
                return Err(invalid("card.member_identity"));
            }
            if member.lifecycle == MemberLifecycle::Active {
                if Some(member.observation_id) != self.item.gallery_observation_id {
                    return Err(invalid("card.gallery_observation"));
                }
                active.insert(member.member_id);
            }
        }
        if active != self.item.member_order.iter().copied().collect()
            || active.len() != self.item.member_order.len()
        {
            return Err(invalid("card.member_order"));
        }
        Ok(())
    }
    pub fn title(&self) -> Result<Option<String>> {
        Ok(project_metadata(&self.item, true, self.observation.as_ref())?.title)
    }
    pub fn member(&self, id: MemberId) -> Option<&GalleryMember> {
        self.members
            .iter()
            .find(|member| member.member_id == id && member.lifecycle == MemberLifecycle::Active)
    }
    pub fn cover(&self) -> Option<MemberId> {
        match self.item.cover_override {
            Override::Set(id) => Some(id),
            Override::Clear => None,
            Override::Inherit => self.item.member_order.first().copied(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaPurpose {
    Preview,
    Playback,
}

/// A local intent, not a credential, URL, texture or player handle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaTicket {
    generation: u64,
    serial: u64,
    pub member: MemberRef,
    pub purpose: MediaPurpose,
}

#[derive(Debug, Clone, PartialEq)]
pub enum WallIntent {
    ResolveMedia(MediaTicket),
    StopPlayback(MemberRef),
    /// Host must abort resolvers and dispose cached media for this exact member.
    ReleaseMedia(MemberRef),
    /// Host must release textures/decoded media for every revision of this item.
    InvalidateMedia {
        item_id: ItemId,
    },
    AccessRevoked,
    Inspect(InspectionTicket),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaState {
    NotRequested,
    Loading,
    Ready,
    Unavailable,
    Failed,
}

#[derive(Debug, Clone, PartialEq)]
pub struct InspectionTicket {
    generation: u64,
    serial: u64,
    pub item_id: ItemId,
    pub scope: rss_contracts::ScopeRef,
    pub context: Option<SelectedContext>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CardTicket {
    generation: u64,
    serial: u64,
    pub snapshot: WallSnapshot,
    pub items: Vec<ItemId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagerSource {
    pub source: Source,
    /// Direct selection is independent from visibility inherited through a group.
    pub selected: bool,
    pub visible: bool,
    pub groups: Vec<HostRef>,
    pub state: AcquisitionState,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagerGroup {
    pub group: crate::SourceGroup,
    pub selected: bool,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagerModel {
    pub sources: Vec<ManagerSource>,
    pub groups: Vec<ManagerGroup>,
}
impl ManagerModel {
    pub fn from_library(
        library: &crate::SourceLibrary,
        selection: &crate::ProfileSelection,
        authorized_sources: &BTreeSet<HostRef>,
        states: &BTreeMap<HostRef, AcquisitionState>,
        bounds: &Bounds,
    ) -> Result<Self> {
        let selected = library.selected_sources(selection, authorized_sources)?;
        if library.sources.len() > bounds.max_collection_items
            || library.groups.len() > bounds.max_collection_items
        {
            return Err(invalid("manager.length"));
        }
        let sources = library
            .sources
            .values()
            .filter(|source| {
                source.owner_scope == selection.owner_scope
                    && authorized_sources.contains(&source.source_ref)
            })
            .map(|source| ManagerSource {
                source: source.clone(),
                selected: selection.selected_sources.contains(&source.source_ref),
                visible: selected.contains(&source.source_ref),
                groups: library
                    .memberships
                    .iter()
                    .filter(|(group, id)| {
                        id == &source.source_ref
                            && library
                                .groups
                                .get(group)
                                .is_some_and(|group| group.owner_scope == selection.owner_scope)
                    })
                    .map(|(group, _)| group.clone())
                    .collect(),
                state: states
                    .get(&source.source_ref)
                    .cloned()
                    .unwrap_or(AcquisitionState::NeverStarted),
            })
            .collect();
        let groups = library
            .groups
            .values()
            .filter(|group| group.owner_scope == selection.owner_scope)
            .map(|group| ManagerGroup {
                group: group.clone(),
                selected: selection.selected_groups.contains(&group.group_ref),
            })
            .collect();
        Ok(Self { sources, groups })
    }
}

/// One view's ephemeral state. Reconcile immediately after query replacement or
/// authorization changes, before returning any cached image/player to a renderer.
pub struct WallState {
    cards: BTreeMap<ItemId, WallCard>,
    selected: BTreeMap<ItemId, MemberId>,
    media: BTreeMap<(ItemId, MemberId), MediaState>,
    media_pending: BTreeMap<(ItemId, MemberId), MediaTicket>,
    player: Option<MemberRef>,
    inspection: Option<InspectionTicket>,
    binding: Option<WallSnapshot>,
    denied_snapshot: Option<WallSnapshot>,
    revoked: bool,
    card_pending: BTreeMap<u64, CardTicket>,
    generation: u64,
    serial: u64,
    bounds: Bounds,
}

impl WallState {
    pub fn new(bounds: Bounds) -> Result<Self> {
        bounds.validate()?;
        Ok(Self {
            cards: BTreeMap::new(),
            selected: BTreeMap::new(),
            media: BTreeMap::new(),
            media_pending: BTreeMap::new(),
            player: None,
            inspection: None,
            binding: None,
            denied_snapshot: None,
            revoked: false,
            card_pending: BTreeMap::new(),
            generation: 1,
            serial: 0,
            bounds,
        })
    }
    pub fn card(&self, item: ItemId) -> Option<&WallCard> {
        self.cards.get(&item)
    }
    pub fn player(&self) -> Option<&MemberRef> {
        self.player.as_ref()
    }
    pub fn inspection(&self) -> Option<&InspectionTicket> {
        self.inspection.as_ref()
    }
    pub fn is_revoked(&self) -> bool {
        self.revoked
    }
    pub fn media_state(&self, member: &MemberRef) -> MediaState {
        self.media
            .get(&(member.item_id, member.member_id))
            .cloned()
            .unwrap_or(MediaState::NotRequested)
    }
    pub fn selected_member(&self, item: ItemId) -> Option<&GalleryMember> {
        let card = self.cards.get(&item)?;
        card.member(self.selected.get(&item).copied().or_else(|| card.cover())?)
    }

    pub fn request_cards(
        &mut self,
        query: &QuerySession,
        items: Vec<ItemId>,
    ) -> Result<CardTicket> {
        let snapshot = query.snapshot().ok_or(Error::NotFound)?;
        if self.binding.as_ref() != Some(snapshot) {
            return Err(Error::StaleResult);
        }
        if items.is_empty()
            || items.len() > self.bounds.max_page_items
            || self.card_pending.len() >= self.bounds.max_collection_items
        {
            return Err(invalid("cards.request_length"));
        }
        if self
            .card_pending
            .values()
            .any(|pending| pending.items.iter().any(|id| items.contains(id)))
        {
            return Err(Error::Conflict);
        }
        let visible: BTreeSet<_> = query.rows().iter().map(|row| row.item_id).collect();
        if items.iter().any(|id| !visible.contains(id))
            || items.iter().collect::<BTreeSet<_>>().len() != items.len()
        {
            return Err(Error::Denied);
        }
        let ticket = CardTicket {
            generation: self.generation,
            serial: self.next_serial()?,
            snapshot: snapshot.clone(),
            items,
        };
        self.card_pending.insert(ticket.serial, ticket.clone());
        Ok(ticket)
    }

    pub fn complete_cards(
        &mut self,
        ticket: &CardTicket,
        cards: Result<Vec<WallCard>>,
    ) -> Result<Vec<WallIntent>> {
        if self.card_pending.get(&ticket.serial) != Some(ticket)
            || ticket.generation != self.generation
            || self.binding.as_ref() != Some(&ticket.snapshot)
        {
            return Err(Error::StaleResult);
        }
        let cards = match cards {
            Ok(cards) => cards,
            Err(Error::Denied) => {
                let mut intents = self.revoke();
                intents.push(WallIntent::AccessRevoked);
                return Ok(intents);
            }
            Err(error) => {
                self.card_pending.remove(&ticket.serial);
                return Err(error);
            }
        };
        super::encoded_bound(&cards, &self.bounds)?;
        if cards.len() > self.bounds.max_page_items {
            return Err(invalid("cards.page_length"));
        }
        let mut unique = BTreeSet::new();
        for card in &cards {
            card.validate(&self.bounds)?;
            if card.observation.as_ref().is_some_and(|observation| {
                !ticket
                    .snapshot
                    .query
                    .access_partitions
                    .contains(&observation.access_partition)
            }) {
                return Err(Error::Denied);
            }
            if !ticket.items.contains(&card.item.item_id) || !unique.insert(card.item.item_id) {
                return Err(Error::Denied);
            }
            if self
                .cards
                .get(&card.item.item_id)
                .is_some_and(|old| old.item.revision > card.item.revision)
            {
                return Err(Error::StaleResult);
            }
        }
        if unique.len() != ticket.items.len() {
            return Err(invalid("cards.partial_response"));
        }
        self.card_pending.remove(&ticket.serial);
        let mut intents = vec![];
        for card in cards {
            let item = card.item.item_id;
            if self.cards.get(&item).is_some_and(|old| old != &card) {
                // Canonical media/order changes invalidate pending resolutions.
                if self
                    .player
                    .as_ref()
                    .is_some_and(|player| player.item_id == item)
                {
                    intents.extend(self.stop_player());
                }
                intents.push(WallIntent::InvalidateMedia { item_id: item });
                self.media.retain(|(id, _), _| *id != item);
                self.media_pending.retain(|(id, _), _| *id != item);
                if self
                    .selected
                    .get(&item)
                    .is_some_and(|id| card.member(*id).is_none())
                {
                    self.selected.remove(&item);
                }
            }
            self.cards.insert(item, card);
        }
        Ok(intents)
    }

    /// Host calls this after every query replacement/append; returned stops must
    /// actually dispose the host player/decoder. Texture caches obey the same scope.
    pub fn reconcile(&mut self, query: &QuerySession) -> Vec<WallIntent> {
        if self.revoked && query.snapshot() == self.denied_snapshot.as_ref() {
            return vec![];
        }
        if self.binding.as_ref() != query.snapshot() {
            let stops = self.revoke();
            self.binding = query.snapshot().cloned();
            self.revoked = query.query().is_none();
            return stops;
        }
        let visible: BTreeSet<_> = query.rows().iter().map(|row| row.item_id).collect();
        self.cards.retain(|id, _| visible.contains(id));
        self.selected.retain(|id, _| visible.contains(id));
        self.media.retain(|(id, _), _| visible.contains(id));
        self.media_pending.retain(|(id, _), _| visible.contains(id));
        if self.inspection.as_ref().is_some_and(|ticket| {
            !visible.contains(&ticket.item_id)
                || query
                    .rows()
                    .iter()
                    .find(|row| row.item_id == ticket.item_id)
                    .is_none_or(|row| row.selected_context != ticket.context)
        }) {
            self.inspection = None;
        }
        let stop = self.player.as_ref().is_some_and(|member| {
            !visible.contains(&member.item_id)
                || self
                    .selected_member(member.item_id)
                    .is_none_or(|selected| selected.member_id != member.member_id)
        });
        if stop {
            self.stop_player().into_iter().collect()
        } else {
            vec![]
        }
    }

    fn next_serial(&mut self) -> Result<u64> {
        self.serial = self
            .serial
            .checked_add(1)
            .ok_or_else(|| invalid("wall.serial"))?;
        Ok(self.serial)
    }
    pub fn select_member(&mut self, item: ItemId, member: MemberId) -> Result<Vec<WallIntent>> {
        let card = self.cards.get(&item).ok_or(Error::NotFound)?;
        if card.member(member).is_none() {
            return Err(Error::NotFound);
        }
        self.selected.insert(item, member);
        Ok(
            if self
                .player
                .as_ref()
                .is_some_and(|player| player.item_id == item && player.member_id != member)
            {
                self.stop_player().into_iter().collect()
            } else {
                vec![]
            },
        )
    }

    /// Call only on a visible-card intersection or explicit interaction. No network
    /// runs here; the host authorizes and bounds its resolver before marking Ready.
    pub fn request_preview(&mut self, item: ItemId) -> Result<Option<WallIntent>> {
        let member = self.selected_member(item).ok_or(Error::NotFound)?;
        let reference = MemberRef {
            item_id: item,
            member_id: member.member_id,
        };
        if member.availability.state != AvailabilityState::Available {
            self.media
                .insert((item, reference.member_id), MediaState::Unavailable);
            return Ok(None);
        }
        if self
            .media_pending
            .contains_key(&(item, reference.member_id))
            || self.media_state(&reference) != MediaState::NotRequested
        {
            return Ok(None);
        }
        let ticket = MediaTicket {
            generation: self.generation,
            serial: self.next_serial()?,
            member: reference.clone(),
            purpose: MediaPurpose::Preview,
        };
        self.media
            .insert((item, reference.member_id), MediaState::Loading);
        self.media_pending
            .insert((item, reference.member_id), ticket.clone());
        Ok(Some(WallIntent::ResolveMedia(ticket)))
    }

    /// A click/touch/keyboard activation replaces the prior player. Merely rendering
    /// a video card never constructs a player or emits a playback request.
    pub fn play(&mut self, item: ItemId) -> Result<Vec<WallIntent>> {
        let member = self.selected_member(item).ok_or(Error::NotFound)?;
        if !matches!(member.media_kind, MediaKind::Video | MediaKind::Audio)
            || member.availability.state != AvailabilityState::Available
        {
            return Err(Error::NotFound);
        }
        let reference = MemberRef {
            item_id: item,
            member_id: member.member_id,
        };
        if self.player.as_ref() == Some(&reference) {
            return Ok(vec![]);
        }
        let mut intents: Vec<_> = self.stop_player().into_iter().collect();
        let ticket = MediaTicket {
            generation: self.generation,
            serial: self.next_serial()?,
            member: reference.clone(),
            purpose: MediaPurpose::Playback,
        };
        self.media_pending
            .insert((item, reference.member_id), ticket.clone());
        if self.media_state(&reference) != MediaState::Ready {
            self.media
                .insert((item, reference.member_id), MediaState::Loading);
        }
        self.player = Some(reference);
        intents.push(WallIntent::ResolveMedia(ticket));
        Ok(intents)
    }
    pub fn stop_player(&mut self) -> Option<WallIntent> {
        let member = self.player.take()?;
        if self
            .media_pending
            .get(&(member.item_id, member.member_id))
            .is_some_and(|ticket| ticket.purpose == MediaPurpose::Playback)
        {
            self.media_pending
                .remove(&(member.item_id, member.member_id));
            if self.media_state(&member) == MediaState::Loading {
                self.media.remove(&(member.item_id, member.member_id));
            }
        }
        Some(WallIntent::StopPlayback(member))
    }
    /// Release one current member so an evicted or failed preview can be requested
    /// again. Selection and other members are unchanged; old tickets become stale.
    pub fn release_media(&mut self, member: &MemberRef) -> Result<Vec<WallIntent>> {
        self.cards.get(&member.item_id)
            .and_then(|card| card.member(member.member_id))
            .ok_or(Error::NotFound)?;
        let stopped = if self.player.as_ref() == Some(member) {
            self.stop_player()
        } else { None };
        let key = (member.item_id, member.member_id);
        self.media_pending.remove(&key);
        self.media.remove(&key);
        let mut effects = vec![WallIntent::ReleaseMedia(member.clone())];
        effects.extend(stopped);
        Ok(effects)
    }

    /// Invalidates one exact pending resolver. The host aborts its work separately.
    /// A cancelled unresolved preview can be requested again; ready media is retained.
    pub fn cancel_media(&mut self, ticket: &MediaTicket) -> Result<Vec<WallIntent>> {
        let key = (ticket.member.item_id, ticket.member.member_id);
        if ticket.generation != self.generation
            || self.media_pending.get(&key) != Some(ticket)
            || !self.cards.contains_key(&ticket.member.item_id)
        {
            return Err(Error::StaleResult);
        }
        self.media_pending.remove(&key);
        if self.media_state(&ticket.member) == MediaState::Loading {
            self.media.remove(&key);
        }
        if ticket.purpose == MediaPurpose::Playback && self.player.as_ref() == Some(&ticket.member)
        {
            self.player = None;
            Ok(vec![WallIntent::StopPlayback(ticket.member.clone())])
        } else {
            Ok(vec![])
        }
    }

    pub fn complete_media(&mut self, ticket: &MediaTicket, state: MediaState) -> Result<()> {
        let key = (ticket.member.item_id, ticket.member.member_id);
        if ticket.generation != self.generation
            || self.media_pending.get(&key) != Some(ticket)
            || !self.cards.contains_key(&ticket.member.item_id)
        {
            return Err(Error::StaleResult);
        }
        if !matches!(
            state,
            MediaState::Ready | MediaState::Unavailable | MediaState::Failed
        ) {
            return Err(invalid("media.completion"));
        }
        self.media_pending.remove(&key);
        self.media.insert(key, state.clone());
        if state != MediaState::Ready && self.player.as_ref() == Some(&ticket.member) {
            self.player = None;
        }
        Ok(())
    }

    pub fn inspect(&mut self, query: &QuerySession, item: ItemId) -> Result<WallIntent> {
        let scope = query.query().ok_or(Error::Denied)?.scope.clone();
        let row = query
            .rows()
            .iter()
            .find(|row| row.item_id == item)
            .ok_or(Error::NotFound)?;
        let ticket = InspectionTicket {
            generation: self.generation,
            serial: self.next_serial()?,
            item_id: item,
            scope,
            context: row.selected_context.clone(),
        };
        self.inspection = Some(ticket.clone());
        Ok(WallIntent::Inspect(ticket))
    }
    pub fn validate_inspection(
        &self,
        ticket: &InspectionTicket,
        inspection: &Inspection,
    ) -> Result<()> {
        if self.inspection.as_ref() != Some(ticket) || ticket.generation != self.generation {
            return Err(Error::StaleResult);
        }
        if inspection.item.item_id != ticket.item_id || inspection.scope != ticket.scope {
            return Err(Error::Denied);
        }
        let query = &self.binding.as_ref().ok_or(Error::StaleResult)?.query;
        if inspection
            .captures
            .iter()
            .any(|capture| !query.access_partitions.contains(&capture.access_partition))
            || inspection
                .selected_observation
                .as_ref()
                .is_some_and(|observation| {
                    !query
                        .access_partitions
                        .contains(&observation.access_partition)
                })
        {
            return Err(Error::Denied);
        }
        let allowed: BTreeSet<_> = if let Some(context) = &ticket.context {
            let membership = &context.membership;
            if !inspection
                .memberships
                .iter()
                .any(|candidate| candidate == membership)
            {
                return Err(Error::StaleResult);
            }
            let mut targets = BTreeSet::from([
                TagTarget::Item(ticket.item_id),
                TagTarget::Project(membership.project_ref.clone()),
                TagTarget::Membership(membership.membership_id),
            ]);
            if let Some(folder) = &context.folder {
                targets.insert(TagTarget::Folder(folder.clone()));
            }
            targets
        } else {
            if !inspection.memberships.is_empty() {
                return Err(Error::Denied);
            }
            BTreeSet::from([TagTarget::Item(ticket.item_id)])
        };
        if inspection.targets.iter().any(|target| {
            !allowed.contains(&target.target)
                || match &target.target {
                    TagTarget::Item(_) => target.scope != inspection.item.owner_scope,
                    _ => ticket
                        .context
                        .as_ref()
                        .is_none_or(|context| target.scope != context.membership.scope),
                }
        }) {
            return Err(Error::Denied);
        }
        if inspection.memberships.len() > usize::from(ticket.context.is_some()) {
            return Err(Error::Denied);
        }
        Ok(())
    }
    pub fn close_inspection(&mut self) {
        self.inspection = None;
    }
    pub fn revoke(&mut self) -> Vec<WallIntent> {
        let mut stop: Vec<_> = self.stop_player().into_iter().collect();
        stop.extend(
            self.cards
                .keys()
                .map(|item_id| WallIntent::InvalidateMedia { item_id: *item_id }),
        );
        self.denied_snapshot = self.binding.clone();
        self.revoked = true;
        self.cards.clear();
        self.selected.clear();
        self.media.clear();
        self.media_pending.clear();
        self.inspection = None;
        self.binding = None;
        self.card_pending.clear();
        self.generation = self.generation.saturating_add(1);
        stop
    }
}
