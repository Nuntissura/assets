use std::{cmp::Ordering, collections::BTreeSet};

use rss_contracts::{Bounds, Error, ItemId, Result, WallCursor};
use serde::{Deserialize, Serialize};

use super::{invalid, text, WallQuery, WallRow};

/// Host-owned identity of a stable membership AND order snapshot. A high-water
/// ID over mutable sort keys is insufficient. Expiry must return an error, never
/// silently execute the cursor against current data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WallSnapshot {
    pub token: String,
    pub query: WallQuery,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadingPosition {
    pub item_id: ItemId,
    pub offset_basis_points: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageRequest {
    pub query: WallQuery,
    pub snapshot: Option<WallSnapshot>,
    pub after: Option<WallCursor>,
    /// Refresh seeks this canonical item in the NEW snapshot. Not a loaded-row offset.
    pub around: Option<ReadingPosition>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AnchorResolution {
    NotRequested,
    Preserved(ReadingPosition),
    /// Host canonical lookup proved the item is outside the new query/snapshot.
    Removed(ItemId),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PageHealth {
    Ready,
    Partial { reason: String },
    Reconnect,
    Failed { reason: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WallPage {
    pub snapshot: WallSnapshot,
    pub rows: Vec<WallRow>,
    pub next: Option<WallCursor>,
    pub anchor: AnchorResolution,
    pub health: PageHealth,
}

impl WallPage {
    fn validate(&self, request: &PageRequest, bounds: &Bounds) -> Result<()> {
        request.query.validate(bounds)?;
        super::encoded_bound(self, bounds)?;
        if matches!(
            self.health,
            PageHealth::Reconnect | PageHealth::Failed { .. }
        ) && (!self.rows.is_empty() || self.next.is_some())
        {
            return Err(invalid("page.failure_has_results"));
        }
        text(
            &self.snapshot.token,
            "snapshot.token",
            bounds.max_text_bytes,
        )?;
        if self.snapshot.query != request.query
            || request
                .snapshot
                .as_ref()
                .is_some_and(|snapshot| snapshot != &self.snapshot)
        {
            return Err(Error::StaleResult);
        }
        if self.rows.len() > request.query.page_size.get() {
            return Err(invalid("page.length"));
        }
        if let Some(cursor) = &self.next {
            text(&cursor.0, "wall_cursor", bounds.max_text_bytes)?;
            if self.rows.is_empty() || request.after.as_ref() == Some(cursor) {
                return Err(invalid("page.cursor_progress"));
            }
        }
        let mut ids = BTreeSet::new();
        for row in &self.rows {
            row.validate(&request.query, bounds)?;
            if !ids.insert(row.item_id) {
                return Err(invalid("page.duplicate_item"));
            }
        }
        if self
            .rows
            .windows(2)
            .any(|rows| rows[0].order.compare(&rows[1].order, request.query.sort) != Ordering::Less)
        {
            return Err(invalid("page.order"));
        }
        match (request.around, &self.anchor) {
            (None, AnchorResolution::NotRequested) => {}
            (Some(expected), AnchorResolution::Preserved(actual))
                if expected == *actual
                    && self.rows.iter().any(|row| row.item_id == actual.item_id) => {}
            (Some(expected), AnchorResolution::Removed(actual))
                if expected.item_id == *actual && !ids.contains(actual) => {}
            _ => return Err(invalid("page.anchor")),
        }
        match &self.health {
            PageHealth::Partial { reason } | PageHealth::Failed { reason } => {
                text(reason, "page.health", bounds.max_text_bytes)?
            }
            _ => {}
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RequestKind {
    Initial,
    Next,
    Refresh,
}

/// The opaque generation prevents an old profile/request completion from replacing
/// a new view. Hosts must still reauthorize at the storage/media boundaries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageTicket {
    generation: u64,
    serial: u64,
    kind: RequestKind,
    pub request: PageRequest,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueryPhase {
    Idle,
    Loading,
    Ready,
    Empty,
    Partial(String),
    Reconnect,
    Failed(Error),
    Revoked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshApplied {
    Preserved(ReadingPosition),
    AnchorRemoved(ItemId),
    AtStart,
}

pub struct QuerySession {
    query: WallQuery,
    bounds: Bounds,
    maximum_loaded_items: usize,
    generation: u64,
    serial: u64,
    snapshot: Option<WallSnapshot>,
    rows: Vec<WallRow>,
    next: Option<WallCursor>,
    cursor_history: BTreeSet<String>,
    pending: Option<PageTicket>,
    refreshing: Option<PageTicket>,
    refresh_ready: Option<(Option<ReadingPosition>, WallPage)>,
    position: Option<ReadingPosition>,
    phase: QueryPhase,
}

impl QuerySession {
    pub fn new(query: WallQuery, bounds: Bounds, maximum_loaded_items: usize) -> Result<Self> {
        query.validate(&bounds)?;
        if maximum_loaded_items < query.page_size.get() {
            return Err(invalid("maximum_loaded_items"));
        }
        Ok(Self {
            query,
            bounds,
            maximum_loaded_items,
            generation: 1,
            serial: 0,
            snapshot: None,
            rows: vec![],
            next: None,
            cursor_history: BTreeSet::new(),
            pending: None,
            refreshing: None,
            refresh_ready: None,
            position: None,
            phase: QueryPhase::Idle,
        })
    }
    pub fn query(&self) -> Option<&WallQuery> {
        (self.phase != QueryPhase::Revoked).then_some(&self.query)
    }
    pub fn rows(&self) -> &[WallRow] {
        &self.rows
    }
    pub fn phase(&self) -> &QueryPhase {
        &self.phase
    }
    pub fn snapshot(&self) -> Option<&WallSnapshot> {
        self.snapshot.as_ref()
    }
    pub fn has_next(&self) -> bool {
        self.next.is_some()
    }
    pub fn loading(&self) -> bool {
        self.pending.is_some()
    }
    pub fn refreshing(&self) -> bool {
        self.refreshing.is_some()
    }
    pub fn refresh_available(&self) -> bool {
        self.refresh_ready.is_some()
    }
    pub fn position(&self) -> Option<ReadingPosition> {
        self.position
    }

    pub fn set_position(&mut self, position: ReadingPosition) -> Result<()> {
        if position.offset_basis_points > 10_000
            || !self.rows.iter().any(|row| row.item_id == position.item_id)
        {
            return Err(invalid("reading_position"));
        }
        self.position = Some(position);
        Ok(())
    }

    fn ticket(&mut self, kind: RequestKind, request: PageRequest) -> Result<PageTicket> {
        if self.phase == QueryPhase::Revoked {
            return Err(Error::Denied);
        }
        self.serial = self
            .serial
            .checked_add(1)
            .ok_or_else(|| invalid("request_serial"))?;
        Ok(PageTicket {
            generation: self.generation,
            serial: self.serial,
            kind,
            request,
        })
    }

    pub fn load(&mut self) -> Result<PageTicket> {
        if self.pending.is_some() || self.snapshot.is_some() {
            return Err(Error::Conflict);
        }
        let ticket = self.ticket(
            RequestKind::Initial,
            PageRequest {
                query: self.query.clone(),
                snapshot: None,
                after: None,
                around: None,
            },
        )?;
        self.pending = Some(ticket.clone());
        self.phase = QueryPhase::Loading;
        Ok(ticket)
    }

    pub fn load_next(&mut self) -> Result<PageTicket> {
        if self.pending.is_some() {
            return Err(Error::Conflict);
        }
        let after = self.next.clone().ok_or(Error::NotFound)?;
        let snapshot = self.snapshot.clone().ok_or(Error::NotFound)?;
        if self
            .rows
            .len()
            .checked_add(self.query.page_size.get())
            .is_none_or(|n| n > self.maximum_loaded_items)
        {
            return Err(Error::LimitExceeded {
                field: "loaded_window".into(),
            });
        }
        let ticket = self.ticket(
            RequestKind::Next,
            PageRequest {
                query: self.query.clone(),
                snapshot: Some(snapshot),
                after: Some(after),
                around: None,
            },
        )?;
        self.pending = Some(ticket.clone());
        Ok(ticket)
    }

    /// Acquisition may continue while this request is pending; old pages continue
    /// against their old snapshot until an explicitly applied validated replacement.
    pub fn refresh(&mut self) -> Result<PageTicket> {
        if self.refreshing.is_some() || self.refresh_ready.is_some() || self.snapshot.is_none() {
            return Err(Error::Conflict);
        }
        let ticket = self.ticket(
            RequestKind::Refresh,
            PageRequest {
                query: self.query.clone(),
                snapshot: None,
                after: None,
                around: self.position,
            },
        )?;
        self.refreshing = Some(ticket.clone());
        Ok(ticket)
    }

    pub fn complete(&mut self, ticket: &PageTicket, response: Result<WallPage>) -> Result<()> {
        let current = if ticket.kind == RequestKind::Refresh {
            &self.refreshing
        } else {
            &self.pending
        };
        if current.as_ref() != Some(ticket)
            || ticket.generation != self.generation
            || ticket.request.query != self.query
        {
            return Err(Error::StaleResult);
        }
        let checked = response.and_then(|page| {
            page.validate(&ticket.request, &self.bounds)?;
            Ok(page)
        });
        if ticket.kind == RequestKind::Refresh {
            self.refreshing = None;
        } else {
            self.pending = None;
        }
        let page = match checked {
            Ok(page) => page,
            Err(Error::Denied) => {
                self.revoke();
                return Err(Error::Denied);
            }
            Err(error) => {
                self.phase = if error == Error::ReconnectRequired {
                    QueryPhase::Reconnect
                } else {
                    QueryPhase::Failed(error.clone())
                };
                return Err(error);
            }
        };
        match &page.health {
            PageHealth::Reconnect => {
                self.phase = QueryPhase::Reconnect;
                return Ok(());
            }
            PageHealth::Failed { reason } => {
                self.phase = QueryPhase::Failed(Error::Invalid {
                    field: reason.clone(),
                });
                return Ok(());
            }
            _ => {}
        }
        if ticket.kind == RequestKind::Refresh {
            self.refresh_ready = Some((ticket.request.around, page));
            return Ok(());
        }
        if ticket.kind == RequestKind::Next {
            let seen: BTreeSet<_> = self.rows.iter().map(|row| row.item_id).collect();
            if page.rows.iter().any(|row| seen.contains(&row.item_id))
                || self
                    .rows
                    .last()
                    .zip(page.rows.first())
                    .is_some_and(|(a, b)| {
                        a.order.compare(&b.order, self.query.sort) != Ordering::Less
                    })
                || page
                    .next
                    .as_ref()
                    .is_some_and(|next| self.cursor_history.contains(&next.0))
            {
                self.phase = QueryPhase::Failed(invalid("page.continuity"));
                return Err(invalid("page.continuity"));
            }
        }
        if let Some(after) = &ticket.request.after {
            self.cursor_history.insert(after.0.clone());
        }
        self.accept(page, ticket.kind == RequestKind::Next);
        Ok(())
    }

    fn accept(&mut self, page: WallPage, append: bool) {
        self.phase = match &page.health {
            PageHealth::Ready if page.rows.is_empty() && (!append || self.rows.is_empty()) => {
                QueryPhase::Empty
            }
            PageHealth::Ready => QueryPhase::Ready,
            PageHealth::Partial { reason } => QueryPhase::Partial(reason.clone()),
            PageHealth::Reconnect => QueryPhase::Reconnect,
            PageHealth::Failed { reason } => QueryPhase::Failed(Error::Invalid {
                field: reason.clone(),
            }),
        };
        self.snapshot = Some(page.snapshot);
        self.next = page.next;
        if append {
            self.rows.extend(page.rows);
        } else {
            self.rows = page.rows;
            self.cursor_history.clear();
        }
    }

    pub fn apply_refresh(&mut self) -> Result<RefreshApplied> {
        let (requested_position, page) = self.refresh_ready.take().ok_or(Error::NotFound)?;
        if self.position != requested_position {
            return Err(Error::StaleResult);
        }
        self.generation = self
            .generation
            .checked_add(1)
            .ok_or_else(|| invalid("query_generation"))?;
        self.pending = None;
        let applied = match &page.anchor {
            AnchorResolution::Preserved(position) => {
                self.position = Some(*position);
                RefreshApplied::Preserved(*position)
            }
            AnchorResolution::Removed(item) => {
                self.position = None;
                RefreshApplied::AnchorRemoved(*item)
            }
            AnchorResolution::NotRequested => {
                self.position = None;
                RefreshApplied::AtStart
            }
        };
        self.accept(page, false);
        Ok(applied)
    }

    /// Canonical selection/authorization changes invalidate every outstanding result.
    pub fn reset(&mut self, query: WallQuery) -> Result<()> {
        query.validate(&self.bounds)?;
        let generation = self
            .generation
            .checked_add(1)
            .ok_or_else(|| invalid("query_generation"))?;
        self.clear();
        self.generation = generation;
        self.query = query;
        self.phase = QueryPhase::Idle;
        Ok(())
    }
    pub fn revoke(&mut self) {
        self.clear();
        self.phase = QueryPhase::Revoked;
    }
    fn clear(&mut self) {
        self.rows.clear();
        self.snapshot = None;
        self.next = None;
        self.cursor_history.clear();
        self.pending = None;
        self.refreshing = None;
        self.refresh_ready = None;
        self.position = None;
    }
}
