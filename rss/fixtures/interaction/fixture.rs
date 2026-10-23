#![allow(dead_code)]
use hsscrap::*;
use rss_contracts::{Provider, Source, SourceAppearance, SourceState};
use rss_core::query::*;
use rss_core::{ProfileSelection, SourceLibrary};
use std::collections::{BTreeMap, BTreeSet};

pub fn host(id: &str) -> HostRef {
    HostRef {
        host_instance_id: "synthetic-wall".into(),
        owner_kind: "rss".into(),
        native_id: id.into(),
    }
}
pub fn id<T: std::str::FromStr>(id: u64) -> T
where
    T::Err: std::fmt::Debug,
{
    format!("00000000-0000-0000-0000-{id:012x}")
        .parse()
        .unwrap()
}
pub fn bounds() -> Bounds {
    Bounds {
        max_url_bytes: 4096,
        max_text_bytes: 4096,
        max_collection_items: 1000,
        max_page_items: 100,
        max_operation_bytes: 1_000_000,
    }
}
pub fn at() -> Instant {
    Instant::parse("2026-10-23T08:00:00Z").unwrap()
}
pub fn library() -> (SourceLibrary, ProfileSelection) {
    let scope = ScopeRef("account".into());
    let library = SourceLibrary::empty(scope.clone());
    let source = Source {
        source_ref: host("source"),
        owner_scope: scope.clone(),
        provider: Provider::Syndication,
        original_url: "https://example.org/feed".into(),
        display_name: "Field notes".into(),
        revision: Revision(1),
        paused: false,
        common: Extensions::default(),
    };
    let library = library
        .add_source(&scope, library.revision, source, &bounds())
        .unwrap();
    let selection = ProfileSelection {
        owner_scope: scope,
        profile_ref: host("profile"),
        selected_sources: BTreeSet::from([host("source")]),
        selected_groups: BTreeSet::new(),
    };
    (library, selection)
}
pub fn query(size: PageSize) -> WallQuery {
    let (library, selection) = library();
    WallQuery::from_library(
        &library,
        &selection,
        Revision(1),
        "authority-1".into(),
        &BTreeSet::from([host("source")]),
        BTreeSet::from([ScopeRef("public".into())]),
        None,
        ContextFilter::AllAuthorized,
        WallFilter::default(),
        WallSort::Newest,
        size,
        &bounds(),
    )
    .unwrap()
}
pub fn row(number: u64) -> WallRow {
    let item_id = id(number);
    WallRow {
        item_id,
        order: SortKey {
            priority: 0,
            utc_millis: Some(10_000 - number as i64),
            item_id,
        },
        appearances: vec![SourceAppearance {
            appearance_ref: host(&format!("appearance-{number}")),
            item_id,
            source_ref: host("source"),
            access_partition: ScopeRef("public".into()),
            provider_entry_key: number.to_string(),
            first_seen_at: None,
            last_seen_at: None,
            latest_observation_id: None,
            source_state: SourceState::Active,
            common: Extensions::default(),
        }],
        selected_context: None,
    }
}
pub fn card(number: u64, kind: MediaKind, count: u64) -> WallCard {
    let item_id = id(number);
    let members: Vec<_> = (0..count)
        .map(|index| GalleryMember {
            member_id: id(number * 100 + index),
            item_id,
            producer_member_key: None,
            observation_id: id(number + 10_000),
            media_kind: kind,
            caption: None,
            observed_mime: None,
            observed_width: Some(800),
            observed_height: Some(if number % 2 == 0 { 1100 } else { 650 }),
            observed_duration_ms: None,
            availability: Availability {
                state: AvailabilityState::Available,
                checked_at: None,
                reason_code: None,
                retry_after: None,
            },
            lifecycle: MemberLifecycle::Active,
            removed_at: None,
            common: Extensions::default(),
        })
        .collect();
    let item = Item {
        item_id,
        owner_scope: ScopeRef("account".into()),
        revision: Revision(1),
        first_capture_id: id(number + 20_000),
        selected_observation_id: None,
        gallery_observation_id: (count > 0).then_some(id(number + 10_000)),
        member_order: members.iter().map(|member| member.member_id).collect(),
        title_override: Override::Set(format!("Field note {number}: a place for useful ideas")),
        description_override: Override::Inherit,
        cover_override: Override::Inherit,
        created_at: at(),
        updated_at: at(),
        lifecycle: Lifecycle::Active,
        deleted_at: None,
        common: Extensions::default(),
    };
    WallCard {
        item,
        observation: None,
        members,
    }
}

/// In-memory frozen canonical-set fixture. This is not a database implementation
/// or evidence for a host's transaction/snapshot durability.
pub struct FixtureDb {
    pub canonical: Vec<WallRow>,
    snapshots: BTreeMap<String, (WallQuery, Vec<WallRow>)>,
    cursors: BTreeMap<String, (String, usize)>,
    serial: u64,
}
impl FixtureDb {
    pub fn new(count: u64) -> Self {
        Self {
            canonical: (1..=count).map(row).collect(),
            snapshots: BTreeMap::new(),
            cursors: BTreeMap::new(),
            serial: 0,
        }
    }
    pub fn page(&mut self, ticket: &PageTicket) -> WallPage {
        let request = &ticket.request;
        let token = if let Some(snapshot) = &request.snapshot {
            snapshot.token.clone()
        } else {
            self.serial += 1;
            let token = format!("snapshot-{}", self.serial);
            let mut rows = self.canonical.clone();
            rows.sort_by(|a, b| a.order.compare(&b.order, request.query.sort));
            self.snapshots
                .insert(token.clone(), (request.query.clone(), rows));
            token
        };
        let (query, rows) = &self.snapshots[&token];
        assert_eq!(query, &request.query);
        let mut anchor = AnchorResolution::NotRequested;
        let start = if let Some(cursor) = &request.after {
            let (snapshot, index) = &self.cursors[&cursor.0];
            assert_eq!(snapshot, &token);
            *index
        } else if let Some(position) = request.around {
            if let Some(index) = rows.iter().position(|row| row.item_id == position.item_id) {
                anchor = AnchorResolution::Preserved(position);
                index
            } else {
                anchor = AnchorResolution::Removed(position.item_id);
                0
            }
        } else {
            0
        };
        let end = (start + request.query.page_size.get()).min(rows.len());
        let selected = rows[start..end].to_vec();
        let next = if end < rows.len() {
            self.serial += 1;
            let cursor = format!("opaque-page-{}", self.serial);
            self.cursors.insert(cursor.clone(), (token.clone(), end));
            Some(rss_contracts::WallCursor(cursor))
        } else {
            None
        };
        WallPage {
            snapshot: WallSnapshot {
                token,
                query: request.query.clone(),
            },
            rows: selected,
            next,
            anchor,
            health: PageHealth::Ready,
        }
    }
}
pub fn loaded(count: u64) -> (QuerySession, WallState, FixtureDb) {
    let mut session = QuerySession::new(query(PageSize::Items25), bounds(), 500).unwrap();
    let mut db = FixtureDb::new(count);
    let ticket = session.load().unwrap();
    session.complete(&ticket, Ok(db.page(&ticket))).unwrap();
    let mut wall = WallState::new(bounds()).unwrap();
    wall.reconcile(&session);
    (session, wall, db)
}
pub fn install(
    session: &QuerySession,
    wall: &mut WallState,
    cards: Vec<WallCard>,
) -> Vec<WallIntent> {
    let ticket = wall
        .request_cards(
            session,
            cards.iter().map(|card| card.item.item_id).collect(),
        )
        .unwrap();
    wall.complete_cards(&ticket, Ok(cards)).unwrap()
}
pub fn inspection(card: &WallCard) -> hsscrap::interaction::Inspection {
    use hsscrap::interaction::{Inspection, TagChoice, TargetAccess};
    Inspection {
        scope: ScopeRef("account".into()),
        item: card.item.clone(),
        selected_observation: card.observation.clone(),
        captures: vec![],
        memberships: vec![],
        assignments: vec![TagAssignment {
            assignment_id: id(90_001),
            tag_ref: TagRef(host("design")),
            target: TagTarget::Item(card.item.item_id),
            scope: ScopeRef("account".into()),
            origin: TagOrigin::User,
            created_at: at(),
            removed_at: None,
            revision: Revision(1),
            common: Extensions::default(),
        }],
        targets: vec![TargetAccess {
            target: TagTarget::Item(card.item.item_id),
            scope: ScopeRef("account".into()),
            label: "Item".into(),
            writable: true,
        }],
        tag_choices: vec![TagChoice {
            tag: TagRef(host("design")),
            label: "design".into(),
        }],
        can_edit_metadata: true,
        item_owner: host("item-owner"),
        owner_fences: vec![ExpectedRevision {
            owner: host("item-owner"),
            revision: card.item.revision,
        }],
        bounds: bounds(),
    }
}
