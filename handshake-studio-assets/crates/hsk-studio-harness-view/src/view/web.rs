//! The web document host on the viewer (lane SMB-WEB-HOST-1): the `web.*` rows run on the viewer's run
//! (`Run::web_command`, module `hsk_studio_harness_core::harness::web_commands`). No drawing and no egui: a panel lane
//! calls these methods; the drive loop calls them today.
use super::Viewer;
use serde_json::Value;

impl Viewer {
    /// One web command: `Ok` is the record's detail, `Err` the refusal code (the record carries `target` and, for
    /// publish and verify, the detail of what happened before the refusal).
    pub fn web_command(&mut self, command_id: &str, params: &Value) -> Result<Value, String> {
        let ok = self.run.web_command(command_id, params);
        let record = self.run.ops().last().cloned().unwrap_or(Value::Null);
        if ok {
            self.say(format!("{command_id} (web revision {})", self.run.web_state()["revision"]));
            Ok(record["detail"].clone())
        } else {
            let code = record["error"].as_str().unwrap_or("command_refused").to_string();
            self.say(format!("{command_id} refused: {code}"));
            Err(code)
        }
    }

    /// The web host's state.
    pub fn web_json(&self) -> Value {
        self.run.web_state()
    }
}
