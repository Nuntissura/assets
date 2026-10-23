//! Versioned browser adapter. Canonical authorization and all external effects belong to the host.
mod boundary;
pub mod library;
pub mod wire;

use boundary::*;
use hsscrap_wasm::HsscrapBridge;
use rss_contracts::Error;
use rss_core::query::*;
use serde_json::{json, Value};
use std::collections::BTreeMap;
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::prelude::*;

enum Handle {
    Page(PageTicket),
    Cards(CardTicket),
    Media(MediaTicket),
    Inspection(InspectionTicket),
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen)]
pub struct RssBridge {
    instance: String,
    next_token: u64,
    query: QuerySession,
    wall: WallState,
    binding: Option<WallSnapshot>,
    handles: BTreeMap<String, Handle>,
    details: Option<(String, HsscrapBridge)>,
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen)]
impl RssBridge {
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(constructor))]
    pub fn new(input: &str) -> Result<Self, String> {
        let initial: wire::Initial = decode(input)?;
        version(initial.version)?;
        bounds(&initial.bounds)?;
        bound(&initial.query, MAX_QUERY)?;
        let instance =
            uuid::Uuid::parse_str(&initial.instance_id).map_err(|_| "invalid_instance")?;
        if instance.is_nil() || instance.to_string() != initial.instance_id {
            return Err("invalid_instance".into());
        }
        if initial.maximum_loaded_items > 1024 {
            return Err("bounds".into());
        }
        let query = QuerySession::new(
            initial.query,
            initial.bounds.clone(),
            initial.maximum_loaded_items,
        )
        .map_err(|_| "invalid_query")?;
        let mut wall = WallState::new(initial.bounds).map_err(|_| "bounds")?;
        wall.reconcile(&query);
        let result = Self {
            instance: initial.instance_id,
            next_token: 0,
            query,
            wall,
            binding: None,
            handles: BTreeMap::new(),
            details: None,
        };
        result.snapshot()?;
        Ok(result)
    }

    pub fn snapshot(&self) -> Result<String, String> {
        encode(&json!({"version":1,"view":self.view()?}))
    }

    pub fn apply(&mut self, input: &str) -> Result<String, String> {
        let command: wire::Command = decode(input)?;
        version(command.version)?;
        use wire::Action;
        if matches!(
            &command.action,
            Action::Load
                | Action::LoadNext
                | Action::Refresh
                | Action::RequestCards { .. }
                | Action::Preview { .. }
                | Action::Play { .. }
                | Action::Inspect { .. }
        ) {
            self.capacity()?;
        }
        let mut effects = vec![];
        let mut applied = Value::Null;
        let result: rss_contracts::Result<()> = match command.action {
            Action::Load => self
                .query
                .load()
                .map(|ticket| self.page_effect(ticket, &mut effects)),
            Action::LoadNext => self
                .query
                .load_next()
                .map(|ticket| self.page_effect(ticket, &mut effects)),
            Action::Refresh => self
                .query
                .refresh()
                .map(|ticket| self.page_effect(ticket, &mut effects)),
            Action::SetPosition { position } => self.query.set_position(position),
            Action::ApplyRefresh => self.query.apply_refresh().map(|position| {
                let cancelled: Vec<_> = self
                    .handles
                    .iter()
                    .filter(|(_, handle)| matches!(handle, Handle::Page(_)))
                    .map(|(token, _)| token.clone())
                    .collect();
                for token in cancelled {
                    self.handles.remove(&token);
                    effects.push(json!({"type":"cancel_host_effect","token":token}));
                }
                applied = match position {
                    RefreshApplied::Preserved(position) => {
                        json!({"type":"preserved","position":position})
                    }
                    RefreshApplied::AnchorRemoved(item) => {
                        json!({"type":"anchor_removed","item_id":item})
                    }
                    RefreshApplied::AtStart => json!({"type":"at_start"}),
                };
            }),
            Action::CompletePage { token, result } => {
                let ticket = match self.handles.get(&token) {
                    Some(Handle::Page(ticket)) => ticket.clone(),
                    _ => return Err("stale_ticket".into()),
                };
                let result = match result {
                    wire::PageCompletion::Page { page } => {
                        // Check the potential retained window before handing an input to the owner.
                        let rows: Vec<_> = if ticket.request.after.is_some() {
                            self.query.rows().iter().chain(page.rows.iter()).collect()
                        } else {
                            page.rows.iter().collect()
                        };
                        bound(&rows, MAX_RETAINED)?;
                        Ok(page)
                    }
                    wire::PageCompletion::Error { error } => Err(error),
                };
                self.handles.remove(&token);
                self.query.complete(&ticket, result)
            }
            Action::RequestCards { items } => {
                self.wall.request_cards(&self.query, items).map(|ticket| {
                    let items = ticket.items.clone();
                    let snapshot = ticket.snapshot.clone();
                    let token = self.retain(Handle::Cards(ticket));
                    effects.push(
                        json!({"type":"cards","token":token,"items":items,"snapshot":snapshot}),
                    );
                })
            }
            Action::CompleteCards { token, result } => {
                let ticket = match self.handles.get(&token) {
                    Some(Handle::Cards(ticket)) => ticket.clone(),
                    _ => return Err("stale_ticket".into()),
                };
                let result = match result {
                    wire::CardCompletion::Cards { cards } => {
                        let replacement: BTreeMap<_, _> =
                            cards.iter().map(|card| (card.item.item_id, card)).collect();
                        let retained: Vec<_> = self
                            .query
                            .rows()
                            .iter()
                            .filter_map(|row| {
                                replacement
                                    .get(&row.item_id)
                                    .copied()
                                    .or_else(|| self.wall.card(row.item_id))
                            })
                            .collect();
                        bound(&retained, MAX_RETAINED)?;
                        Ok(cards)
                    }
                    wire::CardCompletion::Error { error } => Err(error),
                };
                let failed_read = result.is_err();
                let outcome = self.wall.complete_cards(&ticket, result);
                if outcome.is_ok() || failed_read {
                    self.handles.remove(&token);
                }
                match outcome {
                    Ok(intents) => {
                        self.intents(intents, &mut effects);
                        Ok(())
                    }
                    Err(error) => Err(error),
                }
            }
            Action::Preview { item_id } => match self.wall.request_preview(item_id) {
                Ok(intent) => {
                    self.intents(intent.into_iter().collect(), &mut effects);
                    Ok(())
                }
                Err(error) => Err(error),
            },
            Action::Play { item_id } => match self.wall.play(item_id) {
                Ok(intents) => {
                    self.intents(intents, &mut effects);
                    Ok(())
                }
                Err(error) => Err(error),
            },
            Action::Stop => {
                let intent = self.wall.stop_player();
                self.intents(intent.into_iter().collect(), &mut effects);
                Ok(())
            }
            Action::SelectMember { item_id, member_id } => {
                match self.wall.select_member(item_id, member_id) {
                    Ok(intents) => {
                        self.intents(intents, &mut effects);
                        Ok(())
                    }
                    Err(error) => Err(error),
                }
            }
            Action::CompleteMedia { token, state } => {
                let ticket = match self.handles.get(&token) {
                    Some(Handle::Media(ticket)) => ticket.clone(),
                    _ => return Err("stale_ticket".into()),
                };
                let state = match state {
                    wire::MediaCompletion::Ready => MediaState::Ready,
                    wire::MediaCompletion::Unavailable => MediaState::Unavailable,
                    wire::MediaCompletion::Failed => MediaState::Failed,
                };
                self.handles.remove(&token);
                let prior = self.wall.player().cloned();
                let result = self.wall.complete_media(&ticket, state);
                if result.is_ok() && self.wall.player().is_none() {
                    if let Some(member) = prior {
                        effects.push(json!({"type":"stop_playback","member":member}));
                    }
                }
                result
            }
            Action::Inspect { item_id } => {
                self.close_details(&mut effects)?;
                match self.wall.inspect(&self.query, item_id) {
                    Ok(intent) => {
                        self.intents(vec![intent], &mut effects);
                        Ok(())
                    }
                    Err(error) => Err(error),
                }
            }
            Action::CompleteInspection {
                token,
                details_instance_id,
                inspection,
            } => {
                if self.details.is_some() {
                    return Err("stale_ticket".into());
                }
                let ticket = match self.handles.get(&token) {
                    Some(Handle::Inspection(ticket)) => ticket.clone(),
                    _ => return Err("stale_ticket".into()),
                };
                let native = inspection.clone().into_native()?;
                match self.wall.validate_inspection(&ticket, &native) {
                    Ok(()) => {
                        let bridge = HsscrapBridge::new(&encode(&hsscrap_wasm::wire::Initial {
                            version: 1,
                            instance_id: details_instance_id,
                            inspection,
                        })?)?;
                        self.details = Some((token, bridge));
                        Ok(())
                    }
                    Err(error) => Err(error),
                }
            }
            Action::CloseInspection => {
                self.close_details(&mut effects)?;
                Ok(())
            }
            Action::Cancel { token } => self.cancel(&token, &mut effects)?,
            Action::Reset { query } => {
                bound(&query, MAX_QUERY)?;
                let result = self.query.reset(query);
                if result.is_ok() {
                    self.clear_handles(&mut effects);
                }
                result
            }
            Action::Revoke => {
                self.query.revoke();
                self.clear_handles(&mut effects);
                Ok(())
            }
        };
        self.reconcile(&mut effects);
        encode(
            &json!({"version":1,"view":self.view()?,"effects":effects,"error":result.err(),"refresh_applied":applied}),
        )
    }

    pub fn details_apply(&mut self, input: &str) -> Result<String, String> {
        let command: wire::Details = decode(input)?;
        version(command.version)?;
        let ticket = match self.handles.get(&command.token) {
            Some(Handle::Inspection(ticket)) if self.wall.inspection() == Some(ticket) => {
                ticket.clone()
            }
            _ => return Err("stale_ticket".into()),
        };
        let action: hsscrap_wasm::wire::Command = decode(&command.input)?;
        if let hsscrap_wasm::wire::Action::Reload { inspection } = &action.action {
            self.wall
                .validate_inspection(&ticket, &inspection.clone().into_native()?)
                .map_err(|_| "inspection_context")?;
        }
        let details = self
            .details
            .as_mut()
            .filter(|(token, _)| token == &command.token)
            .ok_or("stale_ticket")?;
        let result = details.1.apply(&command.input)?;
        let result: Value = serde_json::from_str(&result).map_err(|_| "invalid_output")?;
        let mut effects = vec![];
        if result["view"]["phase"] == "revoked" {
            self.query.revoke();
            self.clear_handles(&mut effects);
        }
        self.reconcile(&mut effects);
        encode(&json!({"version":1,"details":result,"view":self.view()?,"effects":effects}))
    }

    pub fn details_panel(&mut self, input: &str) -> Result<String, String> {
        let command: wire::Details = decode(input)?;
        version(command.version)?;
        let details = self
            .details
            .as_mut()
            .filter(|(token, _)| token == &command.token)
            .ok_or("stale_ticket")?;
        details.1.panel(&command.input)
    }

    pub fn compose_query(input: &str) -> Result<String, String> {
        library::compose(input)
    }
}

impl RssBridge {
    fn capacity(&self) -> Result<(), String> {
        if self.handles.len() >= MAX_HANDLES || self.next_token == u64::MAX {
            Err("backpressure".into())
        } else {
            Ok(())
        }
    }
    fn retain(&mut self, handle: Handle) -> String {
        // capacity was checked before invoking any owner that can create a handle.
        self.next_token += 1;
        let token = format!("{}:{}", self.instance, self.next_token);
        self.handles.insert(token.clone(), handle);
        token
    }
    fn page_effect(&mut self, ticket: PageTicket, effects: &mut Vec<Value>) {
        let request = ticket.request.clone();
        let token = self.retain(Handle::Page(ticket));
        effects.push(json!({"type":"query","token":token,"request":request}));
    }
    fn intents(&mut self, intents: Vec<WallIntent>, effects: &mut Vec<Value>) {
        for intent in intents {
            match intent {
                WallIntent::ResolveMedia(ticket) => {
                    let member = ticket.member.clone();
                    let cancelled: Vec<_> = self.handles.iter().filter(|(_, handle)| matches!(handle, Handle::Media(old) if old.member == member))
                        .map(|(token,_)| token.clone()).collect();
                    for token in cancelled {
                        self.handles.remove(&token);
                        effects.push(json!({"type":"cancel_host_effect","token":token}));
                    }
                    let purpose = match ticket.purpose {
                        MediaPurpose::Preview => "preview",
                        MediaPurpose::Playback => "playback",
                    };
                    let token = self.retain(Handle::Media(ticket));
                    effects.push(
                        json!({"type":"media","token":token,"member":member,"purpose":purpose}),
                    );
                }
                WallIntent::StopPlayback(member) => {
                    self.handles.retain(|_, handle| !matches!(handle, Handle::Media(ticket) if ticket.member == member && ticket.purpose == MediaPurpose::Playback));
                    effects.push(json!({"type":"stop_playback","member":member}));
                }
                WallIntent::InvalidateMedia { item_id } => {
                    self.handles.retain(|_, handle| !matches!(handle, Handle::Media(ticket) if ticket.member.item_id == item_id));
                    effects.push(json!({"type":"invalidate_media","item_id":item_id}));
                }
                WallIntent::AccessRevoked => {
                    self.query.revoke();
                    effects.push(json!({"type":"access_revoked"}));
                }
                WallIntent::Inspect(ticket) => {
                    self.handles
                        .retain(|_, handle| !matches!(handle, Handle::Inspection(_)));
                    let (item, scope, context) =
                        (ticket.item_id, ticket.scope.clone(), ticket.context.clone());
                    let token = self.retain(Handle::Inspection(ticket));
                    effects.push(json!({"type":"inspection","token":token,"item_id":item,"scope":scope,"context":context}));
                }
            }
        }
    }
    fn clear_handles(&mut self, effects: &mut Vec<Value>) {
        for token in self.handles.keys() {
            effects.push(json!({"type":"cancel_host_effect","token":token}));
        }
        self.handles.clear();
        self.details = None;
    }
    fn reconcile(&mut self, effects: &mut Vec<Value>) {
        if self.binding.as_ref() != self.query.snapshot() || self.query.query().is_none() {
            self.clear_handles(effects);
            self.binding = self.query.snapshot().cloned();
        }
        let intents = self.wall.reconcile(&self.query);
        self.intents(intents, effects);
        if self.wall.inspection().is_none() {
            self.details = None;
        }
    }
    fn close_details(&mut self, effects: &mut Vec<Value>) -> Result<(), String> {
        if let Some((_, details)) = &self.details {
            let snapshot: Value =
                serde_json::from_str(&details.snapshot()?).map_err(|_| "invalid_output")?;
            if !snapshot["view"]["draft"].is_null() {
                return Err("draft_pending".into());
            }
        }
        self.details = None;
        self.wall.close_inspection();
        let retired: Vec<_> = self
            .handles
            .iter()
            .filter(|(_, handle)| matches!(handle, Handle::Inspection(_)))
            .map(|(token, _)| token.clone())
            .collect();
        for token in retired {
            self.handles.remove(&token);
            effects.push(json!({"type":"cancel_host_effect","token":token}));
        }
        Ok(())
    }
    fn cancel(
        &mut self,
        token: &str,
        effects: &mut Vec<Value>,
    ) -> Result<rss_contracts::Result<()>, String> {
        if matches!(self.handles.get(token), Some(Handle::Inspection(_))) {
            self.close_details(effects)?;
            return Ok(Ok(()));
        } else {
            let handle = self.handles.remove(token).ok_or("stale_ticket")?;
            let result = match handle {
                Handle::Page(ticket) => match self.query.complete(&ticket, Err(Error::Cancelled)) {
                    Err(Error::Cancelled) => Ok(()),
                    result => result,
                },
                Handle::Cards(ticket) => {
                    match self.wall.complete_cards(&ticket, Err(Error::Cancelled)) {
                        Err(Error::Cancelled) => Ok(()),
                        Ok(intents) => {
                            self.intents(intents, effects);
                            Ok(())
                        }
                        Err(error) => Err(error),
                    }
                }
                Handle::Media(ticket) => match self.wall.cancel_media(&ticket) {
                    Ok(intents) => {
                        self.intents(intents, effects);
                        Ok(())
                    }
                    Err(error) => Err(error),
                },
                Handle::Inspection(_) => Err(Error::StaleResult),
            };
            effects.push(json!({"type":"cancel_host_effect","token":token}));
            return Ok(result);
        }
    }
    fn view(&self) -> Result<Value, String> {
        let phase = match self.query.phase() {
            QueryPhase::Idle => "idle",
            QueryPhase::Loading => "loading",
            QueryPhase::Ready => "ready",
            QueryPhase::Empty => "empty",
            QueryPhase::Partial(_) => "partial",
            QueryPhase::Reconnect => "reconnect",
            QueryPhase::Failed(_) => "failed",
            QueryPhase::Revoked => "revoked",
        };
        let error = match self.query.phase() {
            QueryPhase::Failed(error) => Some(error.clone()),
            QueryPhase::Reconnect => Some(Error::ReconnectRequired),
            QueryPhase::Revoked => Some(Error::Denied),
            _ => None,
        };
        let partial_reason = match self.query.phase() {
            QueryPhase::Partial(reason) => Some(reason),
            _ => None,
        };
        let cards: Vec<_> = self
            .query
            .rows()
            .iter()
            .filter_map(|row| self.wall.card(row.item_id))
            .collect();
        let media: Vec<_> = self
            .query
            .rows()
            .iter()
            .filter_map(|row| {
                self.wall.selected_member(row.item_id).map(|member| {
                    let member = hsscrap::MemberRef {
                        item_id: row.item_id,
                        member_id: member.member_id,
                    };
                    let state = match self.wall.media_state(&member) {
                        MediaState::NotRequested => "not_requested",
                        MediaState::Loading => "loading",
                        MediaState::Ready => "ready",
                        MediaState::Unavailable => "unavailable",
                        MediaState::Failed => "failed",
                    };
                    wire::MediaView { member, state }
                })
            })
            .collect();
        let details = self.details.as_ref().map(|(token, details)| -> Result<Value,String> {
            Ok(json!({"token":token,"snapshot":serde_json::from_str::<Value>(&details.snapshot()?).map_err(|_| "invalid_output")?}))
        }).transpose()?;
        Ok(
            json!({"phase":phase,"error":error,"partial_reason":partial_reason,"query":self.query.query(),"snapshot":self.query.snapshot(),"rows":self.query.rows(),
            "cards":cards,"media":media,"player":self.wall.player(),"position":self.query.position(),
            "has_next":self.query.has_next(),"loading":self.query.loading(),"refreshing":self.query.refreshing(),
            "refresh_available":self.query.refresh_available(),"pending_tokens":self.handles.keys().collect::<Vec<_>>(),"details":details}),
        )
    }
}
