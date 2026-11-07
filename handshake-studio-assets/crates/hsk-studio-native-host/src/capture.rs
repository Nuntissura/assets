//! Capture files: PNG frames, SHA-256 frame hashes and the JSON receipt (hand-written, bounded, no serde).
use crate::error::HostError;
use hsk_studio_vulkan::rgba_len;
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::Path;

/// Lowercase hex SHA-256 of `bytes`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// Writes an 8-bit RGBA PNG. The file is written beside the target and renamed into place.
pub fn write_png(path: &Path, width: u32, height: u32, rgba8: &[u8]) -> Result<(), HostError> {
    let len = rgba_len(width, height)?;
    if rgba8.len() != len {
        return Err(HostError::Io("png_length"));
    }
    let tmp = path.with_extension("png.partial");
    let result = (|| {
        let file = std::fs::File::create(&tmp).map_err(|_| HostError::Io("png_create"))?;
        let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().map_err(|_| HostError::Io("png_header"))?;
        writer.write_image_data(rgba8).map_err(|_| HostError::Io("png_data"))?;
        writer.finish().map_err(|_| HostError::Io("png_finish"))
    })();
    match result {
        Ok(()) => std::fs::rename(&tmp, path).map_err(|_| HostError::Io("png_rename")),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// A JSON value for receipts.
#[derive(Clone, Debug, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Num(u64),
    Float(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    pub fn obj(fields: Vec<(&str, Json)>) -> Self {
        Self::Obj(fields.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
    }
    pub fn str(s: impl Into<String>) -> Self {
        Self::Str(s.into())
    }
    pub fn strs<S: AsRef<str>>(items: &[S]) -> Self {
        Self::Arr(items.iter().map(|s| Self::Str(s.as_ref().to_string())).collect())
    }
    /// Field lookup on an object.
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Self::Obj(fields) => fields.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
    pub fn render(&self) -> String {
        let mut out = String::new();
        self.write(&mut out);
        out
    }
    fn write(&self, out: &mut String) {
        match self {
            Self::Null => out.push_str("null"),
            Self::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Self::Num(n) => out.push_str(&n.to_string()),
            Self::Float(f) if f.is_finite() => out.push_str(&format!("{f}")),
            Self::Float(_) => out.push_str("null"),
            Self::Str(s) => escape(s, out),
            Self::Arr(items) => {
                out.push('[');
                for (i, v) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    v.write(out);
                }
                out.push(']');
            }
            Self::Obj(fields) => {
                out.push('{');
                for (i, (k, v)) in fields.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    escape(k, out);
                    out.push(':');
                    v.write(out);
                }
                out.push('}');
            }
        }
    }
}

fn escape(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Writes `json` and a newline to `path`.
pub fn write_receipt(path: &Path, json: &Json) -> Result<(), HostError> {
    let mut file = std::fs::File::create(path).map_err(|_| HostError::Io("receipt_create"))?;
    file.write_all(json.render().as_bytes())
        .and_then(|()| file.write_all(b"\n"))
        .map_err(|_| HostError::Io("receipt_write"))
}

/// The device block every receipt carries.
pub fn device_json(owner: &hsk_studio_vulkan::Owner) -> Json {
    let d = owner.device_info();
    Json::obj(vec![
        ("name", Json::str(&d.name)),
        ("type", Json::str(d.device_type)),
        ("vendor_id", Json::Num(u64::from(d.vendor_id))),
        ("device_id", Json::Num(u64::from(d.device_id))),
        ("uuid", Json::str(&d.device_uuid)),
        ("api_version", Json::str(format!("{}.{}.{}", d.api_version[0], d.api_version[1], d.api_version[2]))),
        ("driver_name", Json::str(&d.driver_name)),
        ("driver_info", Json::str(&d.driver_info)),
        ("driver_version", Json::Num(u64::from(d.driver_version))),
        ("queue_family", Json::Num(u64::from(d.queue_family))),
    ])
}

pub fn layers_json(owner: &hsk_studio_vulkan::Owner) -> Json {
    let l = owner.layers();
    Json::obj(vec![
        ("disabled", Json::str(l.disabled)),
        ("present", Json::strs(&l.present)),
        ("enabled", Json::strs(&l.enabled)),
    ])
}

/// Writes `text` and a newline to `path` (the gui tree file).
pub fn write_text(path: &Path, text: &str) -> Result<(), HostError> {
    let mut file = std::fs::File::create(path).map_err(|_| HostError::Io("text_create"))?;
    file.write_all(text.as_bytes()).and_then(|()| file.write_all(b"\n")).map_err(|_| HostError::Io("text_write"))
}

/// The status bar's device and backend readout (AC-BEHAV-07): the Owner's device, type and API version.
pub fn device_readout(owner: &hsk_studio_vulkan::Owner) -> String {
    let d = owner.device_info();
    format!("{} ({}), vulkan {}.{}", d.name, d.device_type, d.api_version[0], d.api_version[1])
}
