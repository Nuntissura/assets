//! Item V (SMB-VK-UI-PASS-1): every GPU test of this crate that opens an owner ends with [`assert_clean`].
#![allow(dead_code)]
use hsk_studio_native_host::capture::Json;
use hsk_studio_vulkan::{VALIDATION_LAYER, process_validation};

/// The state a receipt must name: `on` in a debug build whose loader lists the Khronos layer, `absent` in a debug
/// build where it is not listed, `off` in a release build.
pub fn want(receipt: &Json) -> &'static str {
    let name = format!("\"{VALIDATION_LAYER}\"");
    let listed = receipt.get("layers").and_then(|l| l.get("present")).is_some_and(|p| p.render().contains(&name));
    if !cfg!(debug_assertions) {
        "off"
    } else if listed {
        "on"
    } else {
        "absent"
    }
}

/// The receipt names the real state and carries its counts; with the layer on, the process saw zero validation
/// errors (the owner's destruction included). Warnings are printed for the lane file's rulings.
pub fn assert_clean(receipt: &Json) {
    let want = want(receipt);
    assert_eq!(receipt.get("validation").map(Json::render), Some(format!("\"{want}\"")), "{}", receipt.render());
    let report = receipt.get("validation_report").unwrap_or_else(|| panic!("validation_report member: {}", receipt.render()));
    let v = process_validation();
    println!("validation {} errors {} warnings {} first {:?}; receipt {}", v.state, v.errors, v.warnings, v.first, report.render());
    assert_eq!(v.state, want, "{v:?}");
    if want == "on" {
        assert_eq!(report.get("errors").map(Json::render), Some("0".into()), "{}", report.render());
        assert_eq!(v.errors, 0, "validation errors: {v:?}");
    } else {
        println!("NOT_PROVEN: validation layer {VALIDATION_LAYER} (state {want})");
    }
}
