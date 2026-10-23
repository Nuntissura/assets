use serde_json::Value;
use std::{fs, path::Path};

pub(super) fn fixture() -> Value {
    let bytes = fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("testdata")
            .join("ffmpeg-supervision-v1.0.json"),
    )
    .expect("product FFmpeg contract fixture must load");
    serde_json::from_slice(&bytes).expect("product FFmpeg contract fixture must be JSON")
}
