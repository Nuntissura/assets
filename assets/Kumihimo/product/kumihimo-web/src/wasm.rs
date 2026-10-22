use crate::{PilotConfig, PilotHost};
use wasm_bindgen::prelude::*;

fn js_error(error: crate::WebError) -> JsValue {
    let diagnostic = serde_json::to_string(&error)
        .unwrap_or_else(|_| r#"{"code":"boundary_failure","scope":null,"retryable":false}"#.into());
    JsValue::from_str(&diagnostic)
}

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
            return Err(js_error(crate::WebError::new("config_admission")));
        }
        let config: PilotConfig = serde_json::from_str(&config_json)
            .map_err(|_| js_error(crate::WebError::new("invalid_config")))?;
        let host = PilotHost::new(incarnation, config).map_err(js_error)?;
        Ok(Self { host })
    }
    pub fn route_json(&mut self, request: String) -> String {
        self.host.route_json(&request)
    }
}
