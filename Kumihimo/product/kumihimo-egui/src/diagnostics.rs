//! Redacted target/state projections. Provider text and resource keys are omitted.
use crate::*;

pub fn control_id(kind: &str, raw: u64) -> String {
    format!("kumihimo/{kind}/{raw}")
}

#[cfg(feature = "capture")]
pub fn report_json(report: &FrameReport) -> serde_json::Value {
    use serde_json::json;
    json!({
        "adapter": ADAPTER_VERSION, "projection_generation": report.generation.to_string(), "post_action_generation": report.post_generation.to_string(),
        "surface_epoch": report.epoch.to_string(), "regions": report.regions,
        "canonical_open_views": report.canonical_views, "painted_views": report.painted_views,
        "repaint_requested": report.repaint_requested,
        "rejected": report.rejected.iter().map(|s| s.chars().take(256).collect::<String>()).collect::<Vec<_>>(),
        "controls": report.controls.iter().map(|c| json!({
            "id": c.id, "enabled": c.enabled, "target": c.target.map(|v| v.raw().to_string()),
            "bounds_dip": [c.bounds.x(), c.bounds.y(), c.bounds.width(), c.bounds.height()]
        })).collect::<Vec<_>>(),
        "slots": report.slots.iter().map(|s| json!({
            "view": s.view.raw().to_string(), "resource": s.resource.raw().to_string(),
            "frame": s.frame.raw().to_string(), "workspace": s.workspace.raw().to_string(),
            "bounds_dip": [s.bounds.x(), s.bounds.y(), s.bounds.width(), s.bounds.height()],
            "clip_dip": [s.clip.x(), s.clip.y(), s.clip.width(), s.clip.height()],
            "physical_size": s.physical_size, "scale": s.scale, "surface_epoch": s.epoch.to_string(),
            "visible": s.visible, "focused": s.focused
        })).collect::<Vec<_>>()
    })
}
#[cfg(feature = "capture")]
pub fn encode_bounded(value: &serde_json::Value, max_bytes: usize) -> Result<Vec<u8>> {
    struct Bounded {
        bytes: Vec<u8>,
        limit: usize,
    }
    impl std::io::Write for Bounded {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if self
                .bytes
                .len()
                .checked_add(bytes.len())
                .is_none_or(|n| n > self.limit)
            {
                return Err(std::io::Error::other("diagnostic byte limit"));
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut output = Bounded {
        bytes: vec![],
        limit: max_bytes,
    };
    serde_json::to_writer(&mut output, value)
        .map_err(|_| AdapterError::Budget("diagnostic bytes"))?;
    Ok(output.bytes)
}
