//! Optional synchronous JSON bridge. Native editor objects and all effects remain host-owned.
use crate::*;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use wasm_bindgen::prelude::*;

const MAX_WIRE_BYTES: usize = 65_536;
const MAX_WASM_PINS: usize = 1_024;

fn reject(code: &str) -> JsValue {
    JsValue::from_str(code)
}
fn core_error(error: Error) -> JsValue {
    reject(&error.to_string())
}
fn read<T: DeserializeOwned>(json: &str) -> Result<T, JsValue> {
    if json.len() > MAX_WIRE_BYTES {
        return Err(reject("PayloadLimit"));
    }
    // Parser diagnostics can contain input; only a content-free code crosses the boundary.
    serde_json::from_str(json).map_err(|_| reject("InvalidWire"))
}
fn write<T: Serialize>(value: &T) -> Result<String, JsValue> {
    serde_json::to_string(value).map_err(|_| reject("InvalidWire"))
}
fn revision(value: &str) -> Result<u64, JsValue> {
    if value.len() > 20 {
        return Err(reject("InvalidRevision"));
    }
    let parsed = value
        .parse::<u64>()
        .map_err(|_| reject("InvalidRevision"))?;
    if parsed.to_string() != value {
        return Err(reject("InvalidRevision"));
    }
    Ok(parsed)
}

fn budget(value: f64, maximum: usize) -> Result<usize, JsValue> {
    if !value.is_finite() || value.fract() != 0.0 || !(0.0..=maximum as f64).contains(&value) {
        return Err(reject("PayloadLimit"));
    }
    Ok(value as usize)
}
fn channel(value: &str) -> Result<Channel, JsValue> {
    match value {
        "suggestions" => Ok(Channel::Suggestions),
        "search" => Ok(Channel::Search),
        _ => Err(reject("InvalidWire")),
    }
}
fn status(value: &str) -> Result<DeliveryStatus, JsValue> {
    match value {
        "pending" => Ok(DeliveryStatus::Pending),
        "complete" => Ok(DeliveryStatus::Complete),
        "offline" => Ok(DeliveryStatus::Offline),
        "partial" => Ok(DeliveryStatus::Partial),
        "failed" => Ok(DeliveryStatus::Failed),
        "stale_index" => Ok(DeliveryStatus::StaleIndex),
        _ => Err(reject("InvalidWire")),
    }
}
fn delivery_json(delivery: &Delivery) -> Result<String, JsValue> {
    write(&serde_json::json!({"status":delivery.status(),"items":delivery.items()}))
}

/// Bridge-specific decimal counters preserve the existing native foundation wire contract.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FoundationFence {
    context: ContextId,
    generation: String,
    draft_revision: String,
}
impl From<Fence> for FoundationFence {
    fn from(fence: Fence) -> Self {
        Self {
            context: fence.context,
            generation: fence.generation.to_string(),
            draft_revision: fence.draft_revision.to_string(),
        }
    }
}
impl FoundationFence {
    fn decode(self) -> Result<Fence, JsValue> {
        Ok(Fence {
            context: self.context,
            generation: revision(&self.generation)?,
            draft_revision: revision(&self.draft_revision)?,
        })
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FoundationLimits {
    max_text_bytes: u32,
    max_pins: u32,
    max_items: u32,
    max_item_bytes: u32,
    max_actions_per_item: u32,
}
impl FoundationLimits {
    fn decode(self) -> Result<Limits, JsValue> {
        Ok(Limits {
            max_text_bytes: budget(self.max_text_bytes as f64, MAX_WIRE_BYTES)?,
            max_pins: budget(self.max_pins as f64, MAX_WASM_PINS)?,
            max_items: budget(self.max_items as f64, 1024)?,
            max_item_bytes: budget(self.max_item_bytes as f64, MAX_WIRE_BYTES)?,
            max_actions_per_item: budget(self.max_actions_per_item as f64, 64)?,
        })
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FoundationCompletion {
    fence: FoundationFence,
    span: ByteSpan,
    replacement: String,
}

/// Effect-free literal-field foundation; rich native editors use WritingSession.
#[wasm_bindgen(js_name = Session)]
pub struct WasmSession {
    inner: Session,
}
#[wasm_bindgen(js_class = Session)]
impl WasmSession {
    #[wasm_bindgen(constructor)]
    pub fn new(context: &str, limits_json: &str) -> Result<WasmSession, JsValue> {
        let limits: FoundationLimits = read(limits_json)?;
        Ok(Self {
            inner: Session::new(
                ContextId::new(context).map_err(|_| reject("InvalidIdentity"))?,
                limits.decode()?,
            ),
        })
    }
    #[wasm_bindgen(js_name = wireVersion)]
    pub fn wire_version(&self) -> String {
        "handpick.v1".into()
    }
    pub fn draft(&self) -> String {
        self.inner.draft().into()
    }
    #[wasm_bindgen(js_name = draftRevision)]
    pub fn draft_revision(&self) -> String {
        self.inner.draft_revision().to_string()
    }
    #[wasm_bindgen(js_name = setDraft)]
    pub fn set_draft(&mut self, text: String) -> Result<(), JsValue> {
        self.inner.set_draft(text).map_err(core_error)
    }
    pub fn begin(&mut self) -> Result<String, JsValue> {
        write(&FoundationFence::from(
            self.inner.begin().map_err(core_error)?,
        ))
    }
    pub fn deliver(
        &mut self,
        fence_json: &str,
        channel_name: &str,
        status_name: &str,
        items_json: &str,
    ) -> Result<(), JsValue> {
        let fence: FoundationFence = read(fence_json)?;
        self.inner
            .deliver(
                &fence.decode()?,
                channel(channel_name)?,
                status(status_name)?,
                read(items_json)?,
            )
            .map_err(core_error)
    }
    pub fn delivery(&self, channel_name: &str) -> Result<String, JsValue> {
        delivery_json(self.inner.delivery(channel(channel_name)?))
    }
    pub fn select(&mut self, key_json: &str) -> Result<(), JsValue> {
        self.inner.select(read(key_json)?).map_err(core_error)
    }
    pub fn selected(&self) -> Result<String, JsValue> {
        write(&self.inner.selected())
    }
    pub fn complete(&mut self, edit_json: &str) -> Result<u32, JsValue> {
        let edit: FoundationCompletion = read(edit_json)?;
        let caret = self
            .inner
            .complete(CompletionEdit {
                fence: edit.fence.decode()?,
                span: edit.span,
                replacement: edit.replacement,
            })
            .map_err(core_error)?;
        // Constructor limits cap every draft and caret at MAX_WIRE_BYTES.
        Ok(caret as u32)
    }
    pub fn pin(&mut self, pin: &str) -> Result<(), JsValue> {
        self.inner
            .pin(PinId::new(pin).map_err(|_| reject("InvalidIdentity"))?)
            .map_err(core_error)
    }
    pub fn restore(&mut self, pin: &str, occupied_pin: Option<String>) -> Result<(), JsValue> {
        let pin = PinId::new(pin).map_err(|_| reject("InvalidIdentity"))?;
        let occupied = occupied_pin
            .map(PinId::new)
            .transpose()
            .map_err(|_| reject("InvalidIdentity"))?;
        self.inner.restore(&pin, occupied).map_err(core_error)
    }
    pub fn pins(&self) -> Result<String, JsValue> {
        let pins: Vec<_> = self
            .inner
            .pins()
            .iter()
            .map(
                |pin| serde_json::json!({"id":pin.id().as_str(),"tooltip_text":pin.tooltip_text()}),
            )
            .collect();
        write(&pins)
    }
    #[wasm_bindgen(js_name = switchContext)]
    pub fn switch_context(&mut self, context: &str) -> Result<(), JsValue> {
        self.inner
            .switch_context(ContextId::new(context).map_err(|_| reject("InvalidIdentity"))?)
            .map_err(core_error)
    }
    pub fn revoke(&mut self) {
        self.inner.revoke();
    }
}

/// Owns orchestration only; hosts retain the native editor, selection, undo and recovery.
#[wasm_bindgen(js_name = WritingSession)]
pub struct WasmWritingSession {
    inner: WritingSession,
}

#[wasm_bindgen(js_class = WritingSession)]
impl WasmWritingSession {
    /// Context/session/editor identities must come from the authorized host owner.
    #[wasm_bindgen(constructor)]
    pub fn new(context: &str, session: &str, max_pins: f64) -> Result<WasmWritingSession, JsValue> {
        if !max_pins.is_finite()
            || max_pins.fract() != 0.0
            || !(0.0..=MAX_WASM_PINS as f64).contains(&max_pins)
        {
            return Err(reject("PinLimit"));
        }
        let context = ContextId::new(context).map_err(|_| reject("InvalidIdentity"))?;
        let session = SessionId::new(session).map_err(|_| reject("InvalidIdentity"))?;
        Ok(Self {
            inner: WritingSession::new(context, session, max_pins as usize),
        })
    }
    #[wasm_bindgen(js_name = wireVersion)]
    pub fn wire_version(&self) -> String {
        "handpick.v1".into()
    }
    pub fn bind(&mut self, editor: &str, editor_revision: &str) -> Result<(), JsValue> {
        let editor = EditorId::new(editor).map_err(|_| reject("InvalidIdentity"))?;
        self.inner
            .bind(editor, revision(editor_revision)?)
            .map_err(core_error)
    }
    pub fn fence(&self) -> Result<String, JsValue> {
        write(&self.inner.fence().map_err(core_error)?)
    }
    pub fn edited(&mut self, fence_json: &str, editor_revision: &str) -> Result<(), JsValue> {
        self.inner
            .edited(&read(fence_json)?, revision(editor_revision)?)
            .map_err(core_error)
    }
    pub fn expand(&mut self) -> Result<(), JsValue> {
        self.inner.expand().map_err(core_error)
    }
    pub fn expanded(&self) -> bool {
        self.inner.expanded()
    }
    pub fn compact(&mut self) -> Result<(), JsValue> {
        self.inner.compact().map_err(core_error)
    }
    #[wasm_bindgen(js_name = canExpand)]
    pub fn can_expand(&self) -> bool {
        self.inner.can_expand()
    }
    #[wasm_bindgen(js_name = expandIfEmpty)]
    pub fn expand_if_empty(&mut self, fence_json: &str) -> Result<bool, JsValue> {
        self.inner
            .expand_if_empty(&read(fence_json)?)
            .map_err(core_error)
    }
    #[wasm_bindgen(js_name = expandForProse)]
    pub fn expand_for_prose(&mut self, fence_json: &str, utf16_units: f64, line_count: f64) -> Result<bool, JsValue> {
        for metric in [utf16_units, line_count] {
            if !metric.is_finite() || metric.fract() != 0.0 || !(0.0..=u32::MAX as f64).contains(&metric) {
                return Err(reject("InvalidWire"));
            }
        }
        self.inner.expand_for_prose(&read(fence_json)?, utf16_units as u32, line_count as u32).map_err(core_error)
    }
    #[wasm_bindgen(js_name = presentContent)]
    pub fn present_content(&mut self, fence_json: &str, utf16_units: f64, line_count: f64, is_empty: JsValue) -> Result<bool, JsValue> {
        for metric in [utf16_units, line_count] {
            if !metric.is_finite() || metric.fract() != 0.0 || !(0.0..=u32::MAX as f64).contains(&metric) {
                return Err(reject("InvalidWire"));
            }
        }
        let is_empty = is_empty.as_bool().ok_or_else(|| reject("InvalidWire"))?;
        self.inner.present_content(&read(fence_json)?, utf16_units as u32, line_count as u32, is_empty).map_err(core_error)
    }
    pub fn deliver(
        &mut self,
        fence_json: &str,
        channel_name: &str,
        status_name: &str,
        items_json: &str,
    ) -> Result<(), JsValue> {
        self.inner
            .deliver(
                &read(fence_json)?,
                channel(channel_name)?,
                status(status_name)?,
                read(items_json)?,
            )
            .map_err(core_error)
    }
    pub fn delivery(&self, channel_name: &str) -> Result<String, JsValue> {
        delivery_json(self.inner.delivery(channel(channel_name)?))
    }
    pub fn select(&mut self, fence_json: &str, key_json: &str) -> Result<(), JsValue> {
        self.inner
            .select(&read(fence_json)?, read(key_json)?)
            .map_err(core_error)
    }
    pub fn selected(&self) -> Result<String, JsValue> {
        write(&self.inner.selected())
    }
    #[wasm_bindgen(js_name = validateRelationships)]
    pub fn validate_relationships(
        &self,
        page_json: &str,
        provider: &str,
        max_rows: f64,
    ) -> Result<(), JsValue> {
        let page: RelationshipPage = read(page_json)?;
        let provider = ProviderId::new(provider).map_err(|_| reject("InvalidIdentity"))?;
        page.validate(&self.inner, &provider, budget(max_rows, 1024)?)
            .map_err(core_error)
    }
    pub fn park(&mut self, pin: &str) -> Result<(), JsValue> {
        self.inner
            .park(PinId::new(pin).map_err(|_| reject("InvalidIdentity"))?)
            .map_err(core_error)
    }
    pub fn restore(&mut self, pin: &str, replacement_pin: Option<String>) -> Result<(), JsValue> {
        let pin = PinId::new(pin).map_err(|_| reject("InvalidIdentity"))?;
        let replacement = replacement_pin
            .map(PinId::new)
            .transpose()
            .map_err(|_| reject("InvalidIdentity"))?;
        self.inner.restore(&pin, replacement).map_err(core_error)
    }
    #[wasm_bindgen(js_name = beginLookup)]
    pub fn begin_lookup(&mut self) -> Result<String, JsValue> {
        write(&self.inner.begin_lookup().map_err(core_error)?)
    }
    #[wasm_bindgen(js_name = beginComposition)]
    pub fn begin_composition(&mut self, fence_json: &str) -> Result<String, JsValue> {
        write(
            &self
                .inner
                .begin_composition(&read(fence_json)?)
                .map_err(core_error)?,
        )
    }
    #[wasm_bindgen(js_name = endComposition)]
    pub fn end_composition(&mut self, token_json: &str) -> Result<(), JsValue> {
        self.inner
            .end_composition(&read(token_json)?)
            .map_err(core_error)
    }
    #[wasm_bindgen(js_name = validateAction)]
    pub fn validate_action(&self, intent_json: &str) -> Result<(), JsValue> {
        self.inner
            .validate_action(&read(intent_json)?)
            .map_err(core_error)
    }
    #[wasm_bindgen(js_name = validateQuery)]
    pub fn validate_query(&self, provider_json: &str, query_json: &str) -> Result<(), JsValue> {
        let provider: WritingProvider = read(provider_json)?;
        provider
            .validate_query(&self.inner, &read(query_json)?)
            .map_err(core_error)
    }
    pub fn submit(&mut self, intent_json: &str) -> Result<(), JsValue> {
        self.inner.submit(read(intent_json)?).map_err(core_error)
    }
    pub fn acknowledge(&mut self, outcome_json: &str) -> Result<String, JsValue> {
        let result = self
            .inner
            .acknowledge(&read(outcome_json)?)
            .map_err(core_error)?;
        Ok(match result {
            SaveDisposition::CurrentRevisionCommitted => "current_revision_committed",
            SaveDisposition::OlderRevisionCommitted => "older_revision_committed",
            SaveDisposition::RetainActiveComposition => "retain_active_composition",
            SaveDisposition::RetainForRetry => "retain_for_retry",
            SaveDisposition::ReconcileBeforeRetry => "reconcile_before_retry",
        }
        .into())
    }
    pub fn pins(&self) -> Result<String, JsValue> {
        let pins: Vec<_> = self.inner.pins().iter().map(|pin| serde_json::json!({"id": pin.id.as_str(), "editor": pin.editor.as_str(), "revision": pin.revision.to_string()})).collect();
        write(&pins)
    }
    #[wasm_bindgen(js_name = pendingSave)]
    pub fn pending_save(&self) -> Result<String, JsValue> {
        write(&self.inner.pending_save())
    }
    pub fn revoke(&mut self) {
        self.inner.revoke();
    }
}
