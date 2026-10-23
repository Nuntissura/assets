//! External browser shim. Every operation delegates to the published Rust bridge.
use rss_wasm::RssBridge;
use serde_json::{json, Value};
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::prelude::*;

#[cfg_attr(target_arch = "wasm32", wasm_bindgen)]
pub struct Consumer {
    bridge: RssBridge,
}
#[cfg_attr(target_arch = "wasm32", wasm_bindgen)]
impl Consumer {
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(constructor))]
    pub fn new(input: &str) -> Result<Self, String> {
        Ok(Self {
            bridge: RssBridge::new(input)?,
        })
    }
    pub fn snapshot(&self) -> Result<String, String> {
        self.bridge.snapshot()
    }
    pub fn apply(&mut self, input: &str) -> Result<String, String> {
        self.bridge.apply(input)
    }
}
#[cfg_attr(target_arch = "wasm32", wasm_bindgen)]
pub fn example_input() -> String {
    let reference =
        |id: &str| json!({"host_instance_id":"standalone","owner_kind":"example","native_id":id});
    json!({"version":1,"instance_id":"00000000-0000-0000-0000-000000000071","query":{
        "scope":"account","profile":reference("profile"),"library_revision":"9007199254740993","selection_revision":"1","authority_epoch":"example-epoch",
        "branches":[{"source":reference("source"),"group":null,"group_keywords":[],"source_keywords":[],"group_priority":0,"prioritize":false}],
        "access_partitions":["profile-view"],"context":"AllAuthorized","filter":{"search":"","tags":[],"unread_only":false,"bookmarked_only":false},"sort":"Newest","page_size":"Items25"},
        "bounds":{"max_url_bytes":4096,"max_text_bytes":4096,"max_collection_items":100,"max_page_items":25,"max_operation_bytes":100000},"maximum_loaded_items":50}).to_string()
}
pub fn native_check() {
    let mut bridge = Consumer::new(&example_input()).unwrap();
    let initial: Value = serde_json::from_str(&bridge.snapshot().unwrap()).unwrap();
    assert_eq!(initial["view"]["phase"], "idle");
    let loaded: Value = serde_json::from_str(
        &bridge
            .apply(&json!({"version":1,"action":{"type":"load"}}).to_string())
            .unwrap(),
    )
    .unwrap();
    assert_eq!(loaded["view"]["phase"], "loading");
    let query = &loaded["effects"][0]["request"]["query"];
    assert_eq!(query["library_revision"], "9007199254740993");
    let revoked: Value = serde_json::from_str(
        &bridge
            .apply(&json!({"version":1,"action":{"type":"revoke"}}).to_string())
            .unwrap(),
    )
    .unwrap();
    assert_eq!(revoked["view"]["phase"], "revoked");
    assert!(revoked["view"]["query"].is_null());
}
