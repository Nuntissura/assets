use crate::{PilotConfig, PilotHost};
use wasm_bindgen::prelude::*;

/// Minimal string boundary: browser APIs and DOM remain in the host modules.
#[wasm_bindgen]
pub struct BrowserPilot {
    host: PilotHost,
}
#[wasm_bindgen]
impl BrowserPilot {
    #[wasm_bindgen(constructor)]
    pub fn new(incarnation: String, config_json: String) -> Result<BrowserPilot, JsValue> {
        if config_json.len() > crate::wire::MAX_ROUTE_BYTES {
            return Err(JsValue::from_str("config_admission"));
        }
        let config: PilotConfig =
            serde_json::from_str(&config_json).map_err(|_| JsValue::from_str("invalid_config"))?;
        let host =
            PilotHost::new(incarnation, config).map_err(|error| JsValue::from_str(&error.code))?;
        Ok(Self { host })
    }
    pub fn route_json(&mut self, request: String) -> String {
        self.host.route_json(&request)
    }
}
